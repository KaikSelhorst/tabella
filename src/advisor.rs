//! "Suggest index": reads a slow query (as `pg_stat_statements` / the MySQL digest report it,
//! with `$1` / `?` for the values), works out which columns its WHERE, JOIN and ORDER BY use,
//! and proposes the index that would serve them, unless one already does.
//!
//! A heuristic, not a planner: it knows nothing about row counts or value distribution, so it
//! says what the query would *use*, and the plan (EXPLAIN ANALYZE) has the last word.

use std::collections::HashSet;

use crate::db::{Kind, TableStructure};

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// A keyword or an unquoted name, as written.
    Word(String),
    /// A quoted name (`"x"`, `` `x` ``).
    Name(String),
    /// A string literal.
    Str(String),
    /// A placeholder or a number.
    Value,
    /// `= < > <= >= <> !=`
    Op(String),
    Sym(char),
}

fn lex(sql: &str) -> Vec<Tok> {
    let c: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match ch {
            _ if ch.is_whitespace() => i += 1,
            '-' if c.get(i + 1) == Some(&'-') => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            '/' if c.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            '\'' | '"' | '`' => {
                let mut s = String::new();
                i += 1;
                while i < c.len() {
                    if c[i] == ch {
                        if c.get(i + 1) == Some(&ch) {
                            s.push(ch);
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    s.push(c[i]);
                    i += 1;
                }
                i += 1;
                out.push(if ch == '\'' { Tok::Str(s) } else { Tok::Name(s) });
            }
            '$' | '?' | ':' if ch == '?' || c.get(i + 1).is_some_and(|n| n.is_alphanumeric()) => {
                i += 1;
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                    i += 1;
                }
                out.push(Tok::Value);
            }
            _ if ch.is_ascii_digit() => {
                while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                    i += 1;
                }
                out.push(Tok::Value);
            }
            _ if ch.is_alphabetic() || ch == '_' => {
                let start = i;
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '$') {
                    i += 1;
                }
                out.push(Tok::Word(c[start..i].iter().collect()));
            }
            '<' | '>' | '!' | '=' => {
                let two = c.get(i + 1).copied();
                let op = match (ch, two) {
                    ('<' | '>' | '!', Some(next @ '=')) | ('<', Some(next @ '>')) => {
                        i += 1;
                        format!("{ch}{next}")
                    }
                    ('!', _) => {
                        i += 1;
                        out.push(Tok::Sym('!'));
                        continue;
                    }
                    _ => ch.to_string(),
                };
                i += 1;
                out.push(Tok::Op(op));
            }
            _ => {
                out.push(Tok::Sym(ch));
                i += 1;
            }
        }
    }
    out
}

fn is_word(t: &Tok, w: &str) -> bool {
    matches!(t, Tok::Word(x) if x.eq_ignore_ascii_case(w))
}

/// Words that end a table reference or a list of them.
const CLAUSE_ENDS: &[&str] = &[
    "WHERE",
    "GROUP",
    "ORDER",
    "LIMIT",
    "OFFSET",
    "HAVING",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "FETCH",
    "FOR",
    "WINDOW",
    "RETURNING",
    "SET",
    "ON",
    "USING",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "NATURAL",
    "OUTER",
    "LATERAL",
    "VALUES",
];

fn is_clause_end(t: &Tok) -> bool {
    matches!(t, Tok::Word(w) if CLAUSE_ENDS.iter().any(|k| w.eq_ignore_ascii_case(k)))
}

#[derive(Debug, Clone, PartialEq)]
struct TableRef {
    schema: Option<String>,
    table: String,
    alias: Option<String>,
}

#[derive(Debug, Default)]
struct Use {
    eq: Vec<String>,
    range: Vec<String>,
}

/// What a query asks of one table, in the order a composite index would be built.
#[derive(Debug, Clone, PartialEq)]
pub struct TablePlan {
    pub schema: Option<String>,
    pub table: String,
    /// Columns compared with `=`, `IN`, `IS NULL` (also join keys).
    pub eq: Vec<String>,
    /// After them: the first range column, else the ORDER BY columns the index can serve.
    pub tail: Vec<String>,
}

fn name_of(t: &Tok) -> Option<&str> {
    match t {
        Tok::Name(n) => Some(n),
        Tok::Word(w) if !is_clause_end(t) => Some(w),
        _ => None,
    }
}

/// `name` or `qualifier.name` ending at `toks[end]` (exclusive); returns (qualifier, name).
fn column_before(toks: &[Tok], end: usize) -> Option<(Option<String>, String)> {
    let col = name_of(toks.get(end.checked_sub(1)?)?)?;
    if end >= 3 && toks[end - 2] == Tok::Sym('.') {
        let q = name_of(&toks[end - 3])?;
        return Some((Some(q.to_string()), col.to_string()));
    }
    Some((None, col.to_string()))
}

/// `name` or `qualifier.name` starting at `toks[start]`, when it is a column and not a call.
fn column_after(toks: &[Tok], start: usize) -> Option<(Option<String>, String)> {
    let first = name_of(toks.get(start)?)?;
    const NOT_COLUMNS: &[&str] = &[
        "NULL",
        "TRUE",
        "FALSE",
        "CURRENT_DATE",
        "CURRENT_TIMESTAMP",
        "NOW",
        "ANY",
        "ALL",
    ];
    if NOT_COLUMNS.iter().any(|k| first.eq_ignore_ascii_case(k)) {
        return None;
    }
    if toks.get(start + 1) == Some(&Tok::Sym('.')) {
        let col = name_of(toks.get(start + 2)?)?;
        if toks.get(start + 3) == Some(&Tok::Sym('(')) {
            return None;
        }
        return Some((Some(first.to_string()), col.to_string()));
    }
    if toks.get(start + 1) == Some(&Tok::Sym('(')) || toks.get(start + 1) == Some(&Tok::Sym(':')) {
        return None;
    }
    Some((None, first.to_string()))
}

fn table_ref(toks: &[Tok], mut i: usize) -> Option<(TableRef, usize)> {
    let first = name_of(toks.get(i)?)?.to_string();
    i += 1;
    let (schema, table) = if toks.get(i) == Some(&Tok::Sym('.')) {
        let t = name_of(toks.get(i + 1)?)?.to_string();
        i += 2;
        (Some(first), t)
    } else {
        (None, first)
    };
    if toks.get(i).is_some_and(|t| is_word(t, "AS")) {
        i += 1;
    }
    let alias = match toks.get(i) {
        Some(t) if !is_clause_end(t) => name_of(t).map(str::to_string),
        _ => None,
    };
    if alias.is_some() {
        i += 1;
    }
    Some((TableRef { schema, table, alias }, i))
}

/// What `tables_and_conditions` found: the tables, the token ranges of the conditions, and where
/// ORDER BY's items start.
type Statement = (Vec<TableRef>, Vec<(usize, usize)>, Option<usize>);

/// The tables the statement touches, and where its conditions start (`conds`: token ranges of
/// WHERE and every JOIN … ON).
fn tables_and_conditions(toks: &[Tok]) -> Result<Statement, String> {
    let first = match toks.first() {
        Some(Tok::Word(w)) => w.to_ascii_uppercase(),
        _ => return Err("not a statement the advisor reads".into()),
    };
    if !matches!(first.as_str(), "SELECT" | "UPDATE" | "DELETE") {
        return Err("only SELECT, UPDATE and DELETE statements are analysed".into());
    }
    // Subqueries and CTEs change which table a column belongs to.
    for (i, t) in toks.iter().enumerate() {
        if i > 0 && is_word(t, "SELECT") {
            return Err("queries with subqueries aren't analysed".into());
        }
    }
    let mut tables = Vec::new();
    let mut i = match first.as_str() {
        "SELECT" => {
            let mut d = 0;
            let from = toks.iter().position(|t| {
                match t {
                    Tok::Sym('(') => d += 1,
                    Tok::Sym(')') => d -= 1,
                    _ => {}
                }
                d == 0 && is_word(t, "FROM")
            });
            from.ok_or("the statement has no FROM")? + 1
        }
        "UPDATE" => 1,
        _ => {
            toks.iter()
                .position(|t| is_word(t, "FROM"))
                .ok_or("DELETE without FROM")?
                + 1
        }
    };
    let mut conds = Vec::new();
    let mut order_at = None;
    let (first_table, next) = table_ref(toks, i).ok_or("couldn't read the table name")?;
    tables.push(first_table);
    i = next;
    while i < toks.len() {
        let tok = &toks[i];
        if tok == &Tok::Sym(',') {
            if let Some((t, next)) = table_ref(toks, i + 1) {
                tables.push(t);
                i = next;
                continue;
            }
            break;
        }
        if is_word(tok, "JOIN") {
            let Some((t, next)) = table_ref(toks, i + 1) else {
                break;
            };
            tables.push(t);
            i = next;
            continue;
        }
        if is_word(tok, "ON") || is_word(tok, "WHERE") {
            let start = i + 1;
            let mut end = start;
            let mut d = 0;
            while end < toks.len() {
                match &toks[end] {
                    Tok::Sym('(') => d += 1,
                    Tok::Sym(')') => d -= 1,
                    t if d == 0
                        && (is_word(t, "JOIN")
                            || is_word(t, "WHERE")
                            || [
                                "GROUP",
                                "ORDER",
                                "LIMIT",
                                "HAVING",
                                "UNION",
                                "RETURNING",
                                "OFFSET",
                            ]
                            .iter()
                            .any(|k| is_word(t, k))
                            || (is_word(t, "INNER")
                                || is_word(t, "LEFT")
                                || is_word(t, "RIGHT")
                                || is_word(t, "CROSS")
                                || is_word(t, "FULL"))) =>
                    {
                        break;
                    }
                    _ => {}
                }
                end += 1;
            }
            conds.push((start, end));
            i = end;
            continue;
        }
        if is_word(tok, "ORDER") && toks.get(i + 1).is_some_and(|t| is_word(t, "BY")) {
            order_at = Some(i + 2);
            break;
        }
        i += 1;
    }
    // UPDATE t SET … WHERE: the table was read, the WHERE is found by the loop above only when it
    // directly follows; search it explicitly.
    if conds.is_empty()
        && let Some(w) = toks.iter().position(|t| is_word(t, "WHERE"))
    {
        let end = toks[w..]
            .iter()
            .position(|t| {
                ["ORDER", "LIMIT", "RETURNING", "GROUP"]
                    .iter()
                    .any(|k| is_word(t, k))
            })
            .map_or(toks.len(), |p| w + p);
        conds.push((w + 1, end));
    }
    Ok((tables, conds, order_at))
}

/// Reads the statement. One plan per table that has something to index.
pub fn parse(sql: &str) -> Result<Vec<TablePlan>, String> {
    let toks = lex(sql);
    let (tables, conds, order_at) = tables_and_conditions(&toks)?;
    let resolve = |q: &Option<String>, col: &str| -> Option<usize> {
        let _ = col;
        match q {
            Some(q) => tables.iter().position(|t| {
                t.alias.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(q))
                    || t.table.eq_ignore_ascii_case(q)
            }),
            None if tables.len() == 1 => Some(0),
            None => None,
        }
    };
    let mut uses: Vec<Use> = tables.iter().map(|_| Use::default()).collect();
    let push = |list: &mut Vec<String>, c: &str| {
        if !list.iter().any(|x| x.eq_ignore_ascii_case(c)) {
            list.push(c.to_string());
        }
    };
    for (a, b) in conds {
        let part = &toks[a..b.min(toks.len())];
        if part.iter().any(|t| is_word(t, "OR")) {
            return Err(
                "conditions joined with OR aren't analysed (one index rarely serves both sides)".into(),
            );
        }
        for (k, t) in part.iter().enumerate() {
            let negated = k > 0 && is_word(&part[k - 1], "NOT");
            let kind_eq = match t {
                Tok::Op(o) if o == "=" => true,
                Tok::Op(o) if o == "<" || o == ">" || o == "<=" || o == ">=" => false,
                t if is_word(t, "IN") && !negated => true,
                t if is_word(t, "BETWEEN") && !negated => false,
                t if is_word(t, "IS") && part.get(k + 1).is_some_and(|n| is_word(n, "NULL")) => true,
                t if is_word(t, "LIKE")
                    && !negated
                    && matches!(part.get(k + 1), Some(Tok::Str(s)) if !s.starts_with('%') && !s.starts_with('_')) =>
                {
                    false
                }
                _ => continue,
            };
            // `col IS NOT NULL` isn't a lookup: IS followed by NULL only.
            if let Some((q, col)) = column_before(part, k)
                && let Some(ti) = resolve(&q, &col)
            {
                push(
                    if kind_eq {
                        &mut uses[ti].eq
                    } else {
                        &mut uses[ti].range
                    },
                    &col,
                );
            }
            // `a.x = b.y`: both sides are lookups.
            if matches!(t, Tok::Op(o) if o == "=")
                && let Some((q, col)) = column_after(part, k + 1)
                && let Some(ti) = resolve(&q, &col)
            {
                push(&mut uses[ti].eq, &col);
            }
        }
    }
    // ORDER BY the first table's columns, when every item is a plain column in one direction.
    let mut order: Vec<(usize, String)> = Vec::new();
    if let Some(start) = order_at {
        let mut i = start;
        let mut desc_seen = (false, false);
        while let Some((q, col)) = column_after(&toks, i) {
            let consumed = if q.is_some() { 3 } else { 1 };
            let Some(ti) = resolve(&q, &col) else { break };
            order.push((ti, col));
            i += consumed;
            match toks.get(i) {
                Some(t) if is_word(t, "DESC") => {
                    desc_seen.1 = true;
                    i += 1;
                }
                Some(t) if is_word(t, "ASC") => {
                    desc_seen.0 = true;
                    i += 1;
                }
                _ => desc_seen.0 = true,
            }
            if toks.get(i) == Some(&Tok::Sym(',')) {
                i += 1;
            } else {
                break;
            }
        }
        // Mixed directions can't use one plain index.
        if desc_seen.0 && desc_seen.1 {
            order.clear();
        }
    }
    let mut plans = Vec::new();
    for (ti, t) in tables.iter().enumerate() {
        let u = &uses[ti];
        // ORDER BY columns only help the table they all belong to, and not after a range.
        let ordered: Vec<String> = if order.iter().all(|(o, _)| *o == ti) {
            order.iter().map(|(_, c)| c.clone()).collect()
        } else {
            Vec::new()
        };
        let range = u
            .range
            .iter()
            .find(|c| !u.eq.iter().any(|e| e.eq_ignore_ascii_case(c)));
        let tail: Vec<String> = match range {
            Some(r) => vec![r.clone()],
            None => ordered
                .into_iter()
                .filter(|c| !u.eq.iter().any(|e| e.eq_ignore_ascii_case(c)))
                .collect(),
        };
        if u.eq.is_empty() && tail.is_empty() {
            continue;
        }
        plans.push(TablePlan {
            schema: t.schema.clone(),
            table: t.table.clone(),
            eq: u.eq.clone(),
            tail,
        });
    }
    if plans.is_empty() {
        return Err("no WHERE, JOIN or ORDER BY column to build an index on".into());
    }
    Ok(plans)
}

/// The columns an index definition lists, in order (`CREATE INDEX … USING btree (a, b DESC)`,
/// MySQL's `UNIQUE (a, b)`). Empty for a partial index, which doesn't serve every query.
pub fn index_columns(definition: &str) -> Vec<String> {
    if definition.to_ascii_uppercase().contains(" WHERE ") {
        return Vec::new();
    }
    let Some(open) = definition.find('(') else {
        return Vec::new();
    };
    let mut depth = 0;
    let mut cols = Vec::new();
    let mut cur = String::new();
    for ch in definition[open..].chars() {
        match ch {
            '(' => {
                depth += 1;
                if depth > 1 {
                    cur.push(ch);
                }
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                cur.push(ch);
            }
            ',' if depth == 1 => cols.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    cols.push(cur);
    cols.into_iter()
        .map(|c| {
            let c = c.trim();
            let c = c.split_whitespace().next().unwrap_or("");
            c.trim_matches(['"', '`']).to_string()
        })
        .filter(|c| !c.is_empty())
        .collect()
}

/// Whether an index with these leading columns serves the plan: its first columns are the
/// equality columns (in any order), followed by the tail.
fn covers(index: &[String], eq: &[String], tail: &[String]) -> bool {
    let n = eq.len();
    if index.len() < n + tail.len() {
        return false;
    }
    let lead: HashSet<String> = index[..n].iter().map(|c| c.to_lowercase()).collect();
    let want: HashSet<String> = eq.iter().map(|c| c.to_lowercase()).collect();
    lead == want
        && tail
            .iter()
            .zip(&index[n..])
            .all(|(t, i)| t.eq_ignore_ascii_case(i))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Advice {
    /// `schema.table`, as shown.
    pub table: String,
    pub columns: Vec<String>,
    pub ddl: String,
    /// Why these columns, and what the table has already.
    pub reason: String,
}

const MAX_COLUMNS: usize = 4;

/// The first table of the query that lacks a fitting index. `structure` gives a table's columns
/// and indexes (`None`: not found).
pub fn advise(
    kind: Kind,
    default_schema: &str,
    plans: &[TablePlan],
    mut structure: impl FnMut(&str, &str) -> Option<TableStructure>,
) -> Result<Advice, String> {
    let mut covered: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for plan in plans {
        let schema = plan.schema.clone().unwrap_or_else(|| default_schema.to_string());
        let Some(st) = structure(&schema, &plan.table) else {
            missing.push(plan.table.clone());
            continue;
        };
        // Real spelling of the columns; ones the table doesn't have (aliases, expressions) go.
        let real = |c: &String| {
            st.columns
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(c))
                .map(|s| s.name.clone())
        };
        let eq: Vec<String> = plan.eq.iter().filter_map(real).collect();
        let tail: Vec<String> = plan.tail.iter().map_while(real).collect();
        if eq.is_empty() && tail.is_empty() {
            continue;
        }
        let mut cols = eq.clone();
        cols.extend(tail.iter().cloned());
        cols.truncate(MAX_COLUMNS);
        let (eq, tail): (Vec<String>, Vec<String>) = if eq.len() >= cols.len() {
            (cols.clone(), Vec::new())
        } else {
            let n = eq.len();
            (eq, cols[n..].to_vec())
        };
        let existing: Vec<(String, Vec<String>)> = st
            .indexes
            .iter()
            .map(|(n, d)| (n.clone(), index_columns(d)))
            .collect();
        if let Some((name, _)) = existing.iter().find(|(_, c)| covers(c, &eq, &tail)) {
            covered.push(format!("{}.{} has {name}", schema, plan.table));
            continue;
        }
        let partial: Vec<String> = existing
            .iter()
            .filter(|(_, c)| {
                c.first()
                    .is_some_and(|f| cols.iter().any(|x| x.eq_ignore_ascii_case(f)))
            })
            .map(|(n, c)| format!("{n} ({})", c.join(", ")))
            .collect();
        let why = {
            let mut parts = Vec::new();
            if !eq.is_empty() {
                parts.push(format!("looked up by {}", eq.join(", ")));
            }
            if let Some(t) = tail.first() {
                parts.push(
                    if plan.tail.len() == 1 && plan.tail[0].eq_ignore_ascii_case(t) && !plan.eq.is_empty() {
                        format!("then filtered or sorted by {}", tail.join(", "))
                    } else {
                        format!("sorted or filtered by {}", tail.join(", "))
                    },
                );
            }
            parts.join(", ")
        };
        let mut reason = format!(
            "The query reads {}.{} {why}, and no index starts with those columns.",
            schema, plan.table
        );
        if !partial.is_empty() {
            reason.push_str(&format!(" Existing, but not enough: {}.", partial.join("; ")));
        }
        let name: String = format!("idx_{}_{}", plan.table, cols.join("_"))
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
            .take(63)
            .collect();
        let quoted: Vec<String> = cols.iter().map(|c| kind.quote_ident(c)).collect();
        let target = if kind == Kind::Sqlite {
            kind.quote_ident(&plan.table)
        } else {
            kind.qualified(&schema, &plan.table)
        };
        let ddl = format!(
            "CREATE INDEX {} ON {target} ({});",
            kind.quote_ident(&name),
            quoted.join(", ")
        );
        return Ok(Advice {
            table: format!("{schema}.{}", plan.table),
            columns: cols,
            ddl,
            reason,
        });
    }
    if !covered.is_empty() {
        return Err(format!(
            "Already indexed for this query ({}). The time goes elsewhere: run EXPLAIN ANALYZE on it.",
            covered.join("; ")
        ));
    }
    if !missing.is_empty() {
        return Err(format!(
            "Couldn't find {} in this schema: open the query in the editor and check the plan.",
            missing.join(", ")
        ));
    }
    Err("no column of this query can be indexed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::StructColumn;

    fn table(cols: &[&str], indexes: &[(&str, &str)]) -> TableStructure {
        TableStructure {
            columns: cols
                .iter()
                .map(|c| StructColumn {
                    name: c.to_string(),
                    data_type: "text".into(),
                    nullable: true,
                    default: None,
                    pk: false,
                })
                .collect(),
            indexes: indexes
                .iter()
                .map(|(n, d)| (n.to_string(), d.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    fn advice(sql: &str, st: &TableStructure) -> Result<Advice, String> {
        let plans = parse(sql)?;
        advise(Kind::Postgres, "public", &plans, |_, _| Some(st.clone()))
    }

    #[test]
    fn equality_columns_then_the_sort() {
        let st = table(
            &["id", "customer_id", "status", "created_at"],
            &[(
                "orders_pkey",
                "CREATE UNIQUE INDEX orders_pkey ON public.orders USING btree (id)",
            )],
        );
        let a = advice(
            "SELECT * FROM orders WHERE customer_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT 20",
            &st,
        )
        .unwrap();
        assert_eq!(a.columns, ["customer_id", "status", "created_at"]);
        assert_eq!(
            a.ddl,
            "CREATE INDEX \"idx_orders_customer_id_status_created_at\" ON \"public\".\"orders\" (\"customer_id\", \"status\", \"created_at\");"
        );
    }

    #[test]
    fn a_range_column_goes_last_and_ends_the_index() {
        let st = table(&["id", "customer_id", "total", "created_at"], &[]);
        let a = advice(
            "SELECT id FROM orders WHERE created_at >= $1 AND customer_id = $2 AND total > $3 ORDER BY total",
            &st,
        )
        .unwrap();
        assert_eq!(
            a.columns,
            ["customer_id", "created_at"],
            "equality first, one range column"
        );
    }

    #[test]
    fn an_existing_index_means_no_suggestion() {
        let st = table(
            &["id", "email"],
            &[(
                "users_email_key",
                "CREATE UNIQUE INDEX users_email_key ON public.users USING btree (email)",
            )],
        );
        let err = advice("SELECT * FROM users WHERE email = $1", &st).unwrap_err();
        assert!(err.contains("users_email_key"), "{err}");
    }

    #[test]
    fn a_partly_matching_index_is_mentioned() {
        let st = table(
            &["id", "a", "b"],
            &[("t_a", "CREATE INDEX t_a ON public.t USING btree (a)")],
        );
        let a = advice("SELECT * FROM t WHERE a = $1 AND b = $2", &st).unwrap();
        assert_eq!(a.columns, ["a", "b"]);
        assert!(a.reason.contains("t_a (a)"), "{}", a.reason);
        // Order among the equality columns doesn't matter for an existing index.
        let st = table(
            &["a", "b"],
            &[("t_ba", "CREATE INDEX t_ba ON public.t USING btree (b, a)")],
        );
        assert!(advice("SELECT * FROM t WHERE a = $1 AND b = $2", &st).is_err());
    }

    #[test]
    fn joins_resolve_columns_through_aliases() {
        let plans = parse(
            "SELECT o.id FROM orders o JOIN customers c ON c.id = o.customer_id WHERE c.region = ? AND o.status = ?",
        )
        .unwrap();
        let orders = plans.iter().find(|p| p.table == "orders").unwrap();
        assert_eq!(orders.eq, ["customer_id", "status"]);
        let customers = plans.iter().find(|p| p.table == "customers").unwrap();
        assert_eq!(customers.eq, ["id", "region"]);
    }

    #[test]
    fn what_it_will_not_guess() {
        assert!(
            parse("SELECT * FROM t WHERE a = $1 OR b = $2")
                .unwrap_err()
                .contains("OR")
        );
        assert!(
            parse("SELECT * FROM t WHERE a IN (SELECT x FROM u)")
                .unwrap_err()
                .contains("subquer")
        );
        assert!(parse("INSERT INTO t VALUES (1)").is_err());
        assert!(parse("SELECT 1").is_err());
        // Leading wildcard, NOT IN, <>: nothing an index can seek on.
        assert!(parse("SELECT * FROM t WHERE a LIKE '%x' AND b NOT IN (1) AND c <> 2").is_err());
        // Mixed sort directions.
        let p = parse("SELECT * FROM t WHERE a = 1 ORDER BY b ASC, c DESC").unwrap();
        assert!(p[0].tail.is_empty());
    }

    #[test]
    fn updates_and_deletes_are_read_too() {
        let p = parse("UPDATE orders SET status = $1 WHERE customer_id = $2").unwrap();
        assert_eq!(p[0].eq, ["customer_id"]);
        let p = parse("DELETE FROM sessions WHERE expires_at < $1").unwrap();
        assert_eq!(p[0].tail, ["expires_at"]);
    }

    #[test]
    fn index_definitions_give_their_columns() {
        assert_eq!(
            index_columns("CREATE INDEX i ON public.t USING btree (a, \"B\" DESC, c)"),
            ["a", "B", "c"]
        );
        assert_eq!(index_columns("UNIQUE (email)"), ["email"]);
        assert!(
            index_columns("CREATE INDEX i ON t USING btree (a) WHERE (x)").is_empty(),
            "partial"
        );
        assert_eq!(
            index_columns("CREATE INDEX i ON t USING btree (lower(email))"),
            ["lower(email)"]
        );
    }

    #[test]
    fn mysql_quotes_with_backticks() {
        let plans = parse("SELECT * FROM `shop`.`orders` WHERE `customer_id` = ?").unwrap();
        let a = advise(Kind::MySql, "shop", &plans, |_, _| {
            Some(table(&["id", "customer_id"], &[]))
        })
        .unwrap();
        assert_eq!(
            a.ddl,
            "CREATE INDEX `idx_orders_customer_id` ON `shop`.`orders` (`customer_id`);"
        );
    }

    /// "Suggest an index" reads the user's SQL and the server's index definitions: any text.
    #[test]
    fn any_text_is_an_answer() {
        crate::testgen::each_text(24, 3000, |s, _| {
            let _ = (parse(s), index_columns(s));
        });
    }
}
