//! Builders for the statements the UI generates (table browsing and row edits).

use crate::db::Kind;

pub const PAGE_SIZE: usize = 200;

pub struct PageQuery<'a> {
    pub schema: &'a str,
    pub table: &'a str,
    pub filter: &'a str,
    pub order: Option<(&'a str, bool)>,
    /// Primary key: the default order, and the tiebreaker after a user-chosen one.
    pub pk: &'a [String],
    pub page: usize,
}

pub fn select_page(kind: Kind, q: &PageQuery) -> String {
    select_range(kind, q, PAGE_SIZE, q.page * PAGE_SIZE)
}

/// Same query as a page but with any window; exports walk the table in big chunks.
pub fn select_range(kind: Kind, q: &PageQuery, limit: usize, offset: usize) -> String {
    let mut sql = format!("SELECT * FROM {}", kind.qualified(q.schema, q.table));
    if !q.filter.trim().is_empty() {
        // Parenthesized so the filter stays one condition (see `check_filter`).
        sql += &format!(" WHERE ({})", q.filter);
    }
    // Without a stable order Postgres returns an updated row at the end of the table, so after
    // an edit the row would jump away and a neighbor would show up in its place.
    let mut keys: Vec<String> = Vec::new();
    if let Some((col, asc)) = q.order {
        keys.push(format!(
            "{} {}",
            kind.quote_ident(col),
            if asc { "ASC" } else { "DESC" }
        ));
    }
    for p in q.pk {
        if q.order.is_none_or(|(c, _)| c != p) {
            keys.push(format!("{} ASC", kind.quote_ident(p)));
        }
    }
    if !keys.is_empty() {
        sql += &format!(" ORDER BY {}", keys.join(", "));
    }
    sql + &format!(" LIMIT {limit} OFFSET {offset}")
}

/// `INSERT` with only the columns the user filled in; the rest get their defaults.
pub fn insert_values(kind: Kind, schema: &str, table: &str, values: &[(String, Option<String>)]) -> Stmt {
    if values.is_empty() {
        return insert_default(kind, schema, table);
    }
    let cols = values
        .iter()
        .map(|(c, _)| kind.quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = Stmt::new(format!(
        "INSERT INTO {} ({cols}) VALUES (",
        kind.qualified(schema, table)
    ));
    for (i, (_, v)) in values.iter().enumerate() {
        if i > 0 {
            stmt = stmt.sql(", ");
        }
        stmt = stmt.value(v.as_deref());
    }
    stmt.sql(")")
}

pub fn count(kind: Kind, schema: &str, table: &str, filter: &str) -> String {
    let mut sql = format!("SELECT COUNT(*) FROM {}", kind.qualified(schema, table));
    if !filter.trim().is_empty() {
        sql += &format!(" WHERE ({filter})");
    }
    sql
}

/// Quick filters from a cell's context menu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterOp {
    Eq,
    Ne,
}

/// One condition on a column, e.g. `"nome" = 'ana'`. A NULL value becomes IS (NOT) NULL.
pub fn filter_condition(kind: Kind, column: &str, op: FilterOp, value: Option<&str>) -> String {
    let col = kind.quote_ident(column);
    match (op, value) {
        (FilterOp::Eq, None) => format!("{col} IS NULL"),
        (FilterOp::Ne, None) => format!("{col} IS NOT NULL"),
        (FilterOp::Eq, Some(v)) => format!("{col} = {}", kind.quote_str(v)),
        // "Different" keeps NULLs too, which is what people mean.
        (FilterOp::Ne, Some(v)) => format!("({col} <> {} OR {col} IS NULL)", kind.quote_str(v)),
    }
}

fn contains(kind: Kind, col: &str, term: &str) -> String {
    let pattern = kind.quote_str(&format!("%{term}%"));
    match kind {
        Kind::Postgres => format!("CAST({col} AS TEXT) ILIKE {pattern}"),
        // MySQL compares case-insensitively with the default collations.
        Kind::MySql => format!("CAST({col} AS CHAR) LIKE {pattern}"),
    }
}

/// `term` in any of the columns (empty term: no condition).
pub fn search_condition(kind: Kind, columns: &[String], term: &str) -> String {
    let term = term.trim();
    if term.is_empty() || columns.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = columns
        .iter()
        .map(|c| contains(kind, &kind.quote_ident(c), term))
        .collect();
    format!("({})", parts.join(" OR "))
}

/// `filter AND cond`, keeping the user's filter in one piece.
pub fn and(filter: &str, cond: &str) -> String {
    match (filter.trim().is_empty(), cond.trim().is_empty()) {
        (true, _) => cond.trim().to_string(),
        (false, true) => filter.trim().to_string(),
        (false, false) => format!("({}) AND {}", filter.trim(), cond.trim()),
    }
}

/// The WHERE box is pasted into generated SELECT / COUNT queries, and Postgres runs every
/// `;`-separated statement in one call: `1=1; DELETE FROM t; SELECT 1 AS id` would delete rows
/// without any confirmation. So the filter must be a single condition: no `;` and no `--`
/// outside string literals and quoted identifiers.
pub fn check_filter(filter: &str) -> Result<(), String> {
    let chars: Vec<char> = filter.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '\'' | '"' | '`') {
            i += 1;
            while i < chars.len() && chars[i] != c {
                i += 1;
            }
        } else if c == ';' {
            return Err("O filtro aceita uma única condição: \";\" não é permitido.".into());
        } else if c == '-' && chars.get(i + 1) == Some(&'-') {
            return Err("Comentários (--) não são permitidos no filtro.".into());
        }
        i += 1;
    }
    Ok(())
}

/// `INSERT` reproducing one row, for "copy as INSERT".
pub fn insert_row(kind: Kind, table: &str, columns: &[String], row: &[Option<String>]) -> String {
    let cols = columns
        .iter()
        .map(|c| kind.quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let vals = row
        .iter()
        .map(|v| v.as_deref().map_or("NULL".to_string(), |v| kind.quote_str(v)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO {table} ({cols}) VALUES ({vals});")
}

/// Parses what a user typed into a boolean cell. `None` if it isn't a recognizable boolean.
pub fn parse_bool(input: &str) -> Option<bool> {
    match input.trim().to_lowercase().as_str() {
        "true" | "t" | "1" | "yes" | "y" | "on" | "sim" | "s" => Some(true),
        "false" | "f" | "0" | "no" | "n" | "off" | "não" | "nao" => Some(false),
        _ => None,
    }
}

/// A boolean in the form each engine accepts inside quotes: MySQL's `BOOLEAN` is `tinyint(1)`,
/// which rejects `'true'` in strict mode, so it gets `'1'`/`'0'`.
pub fn bool_value(kind: Kind, b: bool) -> &'static str {
    match (kind, b) {
        (Kind::Postgres, true) => "true",
        (Kind::Postgres, false) => "false",
        (Kind::MySql, true) => "1",
        (Kind::MySql, false) => "0",
    }
}

/// Splits a script into statements and strips string literals, quoted identifiers and comments,
/// returning each statement's bare words uppercased. Enough to recognize what a statement does.
fn statement_words(script: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = script.chars().collect();
    let mut statements = vec![Vec::new()];
    let mut word = String::new();
    let mut i = 0;
    let flush = |word: &mut String, statements: &mut Vec<Vec<String>>| {
        if !word.is_empty() {
            statements.last_mut().unwrap().push(word.to_uppercase());
            word.clear();
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            flush(&mut word, &mut statements);
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
            flush(&mut word, &mut statements);
            continue;
        }
        if matches!(c, '\'' | '"' | '`') {
            flush(&mut word, &mut statements);
            i += 1;
            while i < chars.len() && chars[i] != c {
                // Backslash escapes (MySQL, Postgres E'') skip the next char.
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut statements);
            if c == ';' {
                statements.push(Vec::new());
            }
        }
        i += 1;
    }
    flush(&mut word, &mut statements);
    statements.retain(|s| !s.is_empty());
    statements
}

/// What running a script would do, for read-only connections and the "are you sure" prompt.
#[derive(Debug, Default, PartialEq)]
pub struct ScriptRisk {
    /// Anything other than reads (SELECT, SHOW, EXPLAIN...).
    pub writes: bool,
    /// Human descriptions of destructive statements, e.g. "DELETE sem WHERE".
    pub dangers: Vec<String>,
}

pub fn script_risk(script: &str) -> ScriptRisk {
    const READS: &[&str] = &["SELECT", "SHOW", "EXPLAIN", "DESCRIBE", "DESC", "VALUES", "TABLE"];
    const MODIFIES: &[&str] = &["INSERT", "UPDATE", "DELETE", "MERGE"];
    let mut risk = ScriptRisk::default();
    for words in statement_words(script) {
        let first = words[0].as_str();
        let has = |w: &str| words.iter().any(|x| x == w);
        // WITH ... is a read unless one of its parts modifies data (Postgres allows that).
        let read = READS.contains(&first) || (first == "WITH" && !MODIFIES.iter().any(|m| has(m)));
        if !read {
            risk.writes = true;
        }
        match first {
            "TRUNCATE" => risk.dangers.push("TRUNCATE".into()),
            "DROP" | "ALTER" => {
                let object = words.get(1).map(String::as_str).unwrap_or("");
                risk.dangers.push(format!("{first} {object}").trim().to_string());
            }
            "DELETE" | "UPDATE" if !has("WHERE") => risk.dangers.push(format!("{first} sem WHERE")),
            _ => {}
        }
    }
    risk
}

/// Pretty-prints SQL: keywords in caps, one clause per line, short lists kept inline.
pub fn format_sql(kind: Kind, sql: &str) -> String {
    let options = sqlformat::FormatOptions {
        uppercase: Some(true),
        max_inline_arguments: Some(60),
        max_inline_top_level: Some(60),
        dialect: match kind {
            Kind::Postgres => sqlformat::Dialect::PostgreSql,
            Kind::MySql => sqlformat::Dialect::Generic,
        },
        ..Default::default()
    };
    sqlformat::format(sql, &sqlformat::QueryParams::None, &options)
}

/// A statement with its values kept apart from the SQL text.
///
/// MySQL executes it as a prepared statement with `?` parameters, so values never touch the SQL
/// text. Postgres renders the values as `quote_str` literals: the `postgres` crate only sends
/// typed binary parameters, while grid edits are plain text the server must cast itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stmt {
    parts: Vec<Part>,
}

#[derive(Clone, Debug, PartialEq)]
enum Part {
    Sql(String),
    Value(String),
}

impl Stmt {
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            parts: vec![Part::Sql(sql.into())],
        }
    }

    pub fn sql(mut self, sql: impl AsRef<str>) -> Self {
        self.parts.push(Part::Sql(sql.as_ref().to_string()));
        self
    }

    /// A value, or `NULL` written into the SQL when `None`.
    pub fn value(mut self, v: Option<&str>) -> Self {
        self.parts.push(match v {
            Some(v) => Part::Value(v.to_string()),
            None => Part::Sql("NULL".into()),
        });
        self
    }

    pub fn append(mut self, other: Stmt) -> Self {
        self.parts.extend(other.parts);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Values inlined as escaped literals: the Postgres execution path, and for showing the user.
    pub fn render(&self, kind: Kind) -> String {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Sql(s) => s.clone(),
                Part::Value(v) => kind.quote_str(v),
            })
            .collect()
    }

    /// SQL with `?` placeholders plus the values, for a MySQL prepared statement.
    pub fn with_placeholders(&self) -> (String, Vec<String>) {
        let mut sql = String::new();
        let mut values = Vec::new();
        for p in &self.parts {
            match p {
                Part::Sql(s) => sql.push_str(s),
                Part::Value(v) => {
                    sql.push('?');
                    values.push(v.clone());
                }
            }
        }
        (sql, values)
    }
}

/// `pk1 = <v> AND pk2 IS NULL` for the given row.
pub fn pk_where(kind: Kind, pk: &[String], columns: &[String], row: &[Option<String>]) -> Stmt {
    let mut stmt = Stmt::default();
    for p in pk {
        let Some(i) = columns.iter().position(|c| c == p) else {
            continue;
        };
        if !stmt.is_empty() {
            stmt = stmt.sql(" AND ");
        }
        stmt = match &row[i] {
            Some(v) => stmt.sql(format!("{} = ", kind.quote_ident(p))).value(Some(v)),
            None => stmt.sql(format!("{} IS NULL", kind.quote_ident(p))),
        };
    }
    stmt
}

pub fn update_cell(
    kind: Kind,
    schema: &str,
    table: &str,
    column: &str,
    value: Option<&str>,
    wh: Stmt,
) -> Stmt {
    Stmt::new(format!(
        "UPDATE {} SET {} = ",
        kind.qualified(schema, table),
        kind.quote_ident(column)
    ))
    .value(value)
    .sql(" WHERE ")
    .append(wh)
}

/// Inserts a row made only of column defaults.
pub fn insert_default(kind: Kind, schema: &str, table: &str) -> Stmt {
    let t = kind.qualified(schema, table);
    Stmt::new(match kind {
        Kind::Postgres => format!("INSERT INTO {t} DEFAULT VALUES"),
        Kind::MySql => format!("INSERT INTO {t} () VALUES ()"),
    })
}

pub fn delete_row(kind: Kind, schema: &str, table: &str, wh: Stmt) -> Stmt {
    Stmt::new(format!("DELETE FROM {} WHERE ", kind.qualified(schema, table))).append(wh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_query() {
        let q = PageQuery {
            schema: "public",
            table: "t",
            filter: " id > 1 ",
            order: Some(("na\"me", false)),
            pk: &[],
            page: 2,
        };
        assert_eq!(
            select_page(Kind::Postgres, &q),
            "SELECT * FROM \"public\".\"t\" WHERE ( id > 1 ) ORDER BY \"na\"\"me\" DESC LIMIT 200 OFFSET 400"
        );
        let q = PageQuery {
            schema: "db",
            table: "t",
            filter: "",
            order: None,
            pk: &[],
            page: 0,
        };
        assert_eq!(
            select_page(Kind::MySql, &q),
            "SELECT * FROM `db`.`t` LIMIT 200 OFFSET 0"
        );
    }

    #[test]
    fn page_orders_by_primary_key() {
        let pk = vec!["id".to_string()];
        let q = PageQuery {
            schema: "s",
            table: "t",
            filter: "",
            order: None,
            pk: &pk,
            page: 0,
        };
        assert_eq!(
            select_page(Kind::Postgres, &q),
            "SELECT * FROM \"s\".\"t\" ORDER BY \"id\" ASC LIMIT 200 OFFSET 0"
        );
        let q = PageQuery {
            order: Some(("nome", false)),
            ..q
        };
        assert!(select_page(Kind::MySql, &q).contains("ORDER BY `nome` DESC, `id` ASC"));
        let q = PageQuery {
            order: Some(("id", false)),
            ..q
        };
        assert!(select_page(Kind::MySql, &q).ends_with("ORDER BY `id` DESC LIMIT 200 OFFSET 0"));
    }

    #[test]
    fn count_query() {
        assert_eq!(
            count(Kind::Postgres, "s", "t", ""),
            "SELECT COUNT(*) FROM \"s\".\"t\""
        );
        assert_eq!(
            count(Kind::MySql, "s", "t", "id > 1"),
            "SELECT COUNT(*) FROM `s`.`t` WHERE (id > 1)"
        );
    }

    #[test]
    fn insert_copy() {
        let cols = vec!["id".to_string(), "nome".to_string()];
        let row = vec![Some("1".to_string()), None];
        assert_eq!(
            insert_row(Kind::Postgres, "\"public\".\"t\"", &cols, &row),
            "INSERT INTO \"public\".\"t\" (\"id\", \"nome\") VALUES ('1', NULL);"
        );
    }

    #[test]
    fn booleans_per_engine() {
        assert_eq!(parse_bool(" TRUE "), Some(true));
        assert_eq!(parse_bool("f"), Some(false));
        assert_eq!(parse_bool("talvez"), None);
        assert_eq!(bool_value(Kind::Postgres, false), "false");
        assert_eq!(bool_value(Kind::MySql, true), "1");
    }

    #[test]
    fn classifies_scripts() {
        assert_eq!(
            script_risk("select 1; -- drop table x\nSHOW tables"),
            ScriptRisk::default()
        );
        assert_eq!(
            script_risk("WITH a AS (SELECT 1) SELECT * FROM a"),
            ScriptRisk::default()
        );
        let r = script_risk("delete from t; update t set a = 1 where id = 2; DROP TABLE x");
        assert!(r.writes);
        assert_eq!(r.dangers, ["DELETE sem WHERE", "DROP TABLE"]);
        // Words inside strings and comments don't count.
        let r = script_risk("UPDATE t SET note = 'no where here' /* where */");
        assert_eq!(r.dangers, ["UPDATE sem WHERE"]);
        assert!(script_risk("insert into t values (1)").writes);
        assert!(script_risk("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d").writes);
        assert_eq!(script_risk("select ';' ; truncate t").dangers, ["TRUNCATE"]);
    }

    #[test]
    fn filter_must_be_one_condition() {
        assert!(check_filter("id > 10 AND nome LIKE 'a%'").is_ok());
        assert!(check_filter("nome = 'a;b' OR \"we;ird\" = 1").is_ok());
        assert!(check_filter("1=1; DELETE FROM t; SELECT 1 AS id").is_err());
        assert!(check_filter("1=1 -- ").is_err());
        assert!(check_filter("x = 'it''s -- fine'").is_ok());
    }

    #[test]
    fn insert_only_filled_columns() {
        let v = vec![
            ("nome".to_string(), Some("ana".to_string())),
            ("ativo".to_string(), None),
        ];
        let (sql, vals) = insert_values(Kind::MySql, "s", "t", &v).with_placeholders();
        assert_eq!(sql, "INSERT INTO `s`.`t` (`nome`, `ativo`) VALUES (?, NULL)");
        assert_eq!(vals, ["ana"]);
        assert_eq!(
            insert_values(Kind::Postgres, "s", "t", &[]).render(Kind::Postgres),
            "INSERT INTO \"s\".\"t\" DEFAULT VALUES"
        );
    }

    #[test]
    fn formatting() {
        let raw = "select id, nome from clientes c join pedidos p on p.cliente_id = c.id where nome = 'select from' and ativo order by id";
        let out = format_sql(Kind::Postgres, raw);
        assert!(out.starts_with("SELECT"), "{out}");
        assert!(out.lines().count() > 3, "one clause per line:\n{out}");
        assert!(out.contains("'select from'"), "string contents untouched:\n{out}");
        assert_eq!(
            format_sql(Kind::Postgres, &out),
            out,
            "formatting twice changes nothing"
        );
        // Same statements, only whitespace and keyword case differ.
        let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        assert_eq!(squash(&out), squash(raw));
    }

    #[test]
    fn quick_filters() {
        let k = Kind::Postgres;
        assert_eq!(
            filter_condition(k, "nome", FilterOp::Eq, Some("O'Neil")),
            "\"nome\" = 'O''Neil'"
        );
        assert_eq!(filter_condition(k, "x", FilterOp::Eq, None), "\"x\" IS NULL");
        assert_eq!(
            filter_condition(k, "x", FilterOp::Ne, Some("1")),
            "(\"x\" <> '1' OR \"x\" IS NULL)"
        );
        assert_eq!(
            search_condition(Kind::MySql, &["x".into()], "an"),
            "(CAST(`x` AS CHAR) LIKE '%an%')"
        );
        // Generated conditions always pass the WHERE-box check, whatever the value holds.
        for v in ["a;b", "x -- y", "it's", "a\\'; DROP TABLE t; --"] {
            for kind in [Kind::Postgres, Kind::MySql] {
                let c = filter_condition(kind, "c", FilterOp::Eq, Some(v));
                assert!(check_filter(&c).is_ok(), "{kind:?} {c}");
            }
        }
        assert_eq!(and("", "a = 1"), "a = 1");
        assert_eq!(and("b OR c", "a = 1"), "(b OR c) AND a = 1");
        assert_eq!(and("b", ""), "b");
        let s = search_condition(k, &["id".into(), "nome".into()], " ana ");
        assert_eq!(
            s,
            "(CAST(\"id\" AS TEXT) ILIKE '%ana%' OR CAST(\"nome\" AS TEXT) ILIKE '%ana%')"
        );
        assert_eq!(search_condition(k, &["id".into()], "  "), "");
    }

    #[test]
    fn quoting() {
        assert_eq!(Kind::Postgres.quote_str("o'x"), "'o''x'");
        // Backslashes switch to E'' so the literal means the same under any server setting.
        assert_eq!(Kind::Postgres.quote_str("o'x\\"), "E'o''x\\\\'");
        assert_eq!(Kind::MySql.quote_str("o'x\\"), "'o''x\\\\'");
        assert_eq!(Kind::MySql.quote_ident("a`b"), "`a``b`");
    }

    #[test]
    fn row_edits() {
        let cols = vec!["id".to_string(), "name".to_string()];
        let pk = vec!["id".to_string(), "name".to_string()];
        let row = vec![Some("5".to_string()), None];
        let wh = pk_where(Kind::Postgres, &pk, &cols, &row);
        assert_eq!(wh.render(Kind::Postgres), "\"id\" = '5' AND \"name\" IS NULL");
        let up = update_cell(Kind::Postgres, "s", "t", "name", Some("it's"), wh);
        assert_eq!(
            up.render(Kind::Postgres),
            "UPDATE \"s\".\"t\" SET \"name\" = 'it''s' WHERE \"id\" = '5' AND \"name\" IS NULL"
        );
        assert_eq!(
            insert_default(Kind::Postgres, "s", "t").render(Kind::Postgres),
            "INSERT INTO \"s\".\"t\" DEFAULT VALUES"
        );
        assert_eq!(
            insert_default(Kind::MySql, "s", "t").render(Kind::MySql),
            "INSERT INTO `s`.`t` () VALUES ()"
        );
    }

    #[test]
    fn mysql_binds_values_as_parameters() {
        let cols = vec!["id".to_string()];
        let row = vec![Some("5".to_string())];
        let wh = pk_where(Kind::MySql, &cols, &cols, &row);
        let (sql, values) = update_cell(Kind::MySql, "s", "t", "nome", Some("a'b\\"), wh).with_placeholders();
        assert_eq!(sql, "UPDATE `s`.`t` SET `nome` = ? WHERE `id` = ?");
        assert_eq!(values, ["a'b\\", "5"]);
        let (sql, values) = delete_row(
            Kind::MySql,
            "s",
            "t",
            pk_where(Kind::MySql, &cols, &cols, &[None]),
        )
        .with_placeholders();
        assert_eq!(sql, "DELETE FROM `s`.`t` WHERE `id` IS NULL");
        assert!(values.is_empty());
    }
}
