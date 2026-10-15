//! Builders for the statements the UI generates (table browsing and row edits).

use crate::db::Kind;

/// Rows per page in table tabs.
pub fn page_size() -> usize {
    200
}

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
    let n = page_size();
    select_range(kind, q, n, q.page * n)
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

/// The next `limit` rows after `after` (the last primary key read), in key order. Costs the
/// same on any chunk, where OFFSET reads and throws away every row before it.
pub fn select_after(kind: Kind, q: &PageQuery, limit: usize, after: Option<&[Option<String>]>) -> String {
    let keys =
        q.pk.iter()
            .map(|p| kind.quote_ident(p))
            .collect::<Vec<_>>()
            .join(", ");
    let mut conds = Vec::new();
    if !q.filter.trim().is_empty() {
        conds.push(format!("({})", q.filter));
    }
    if let Some(last) = after {
        let vals = last
            .iter()
            .map(|v| v.as_deref().map_or("NULL".to_string(), |v| kind.quote_str(v)))
            .collect::<Vec<_>>()
            .join(", ");
        conds.push(format!("({keys}) > ({vals})"));
    }
    let wh = if conds.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conds.join(" AND "))
    };
    format!(
        "SELECT * FROM {}{wh} ORDER BY {keys} LIMIT {limit}",
        kind.qualified(q.schema, q.table)
    )
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
        // SQLite's LIKE ignores case (for ASCII letters).
        Kind::Sqlite => format!("CAST({col} AS TEXT) LIKE {pattern}"),
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
        // SQLite has no boolean type: 1 and 0 are what it stores.
        (Kind::MySql | Kind::Sqlite, true) => "1",
        (Kind::MySql | Kind::Sqlite, false) => "0",
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

/// Char ranges of the statements in `script`, trimmed of surrounding whitespace and without the
/// `;`. Quotes, comments and Postgres `$tag$` bodies are skipped over, so a `;` inside them
/// doesn't split. With `blank_lines`, an empty line also ends a statement (handy for scratch
/// files without semicolons; a statement under the cursor rarely spans blank lines).
pub fn statement_ranges(script: &str, blank_lines: bool) -> Vec<(usize, usize)> {
    let chars: Vec<char> = script.chars().collect();
    let mut ranges = Vec::new();
    let mut start = 0;
    let push = |a: usize, b: usize, ranges: &mut Vec<(usize, usize)>| {
        let mut a = a;
        let mut b = b;
        while a < b && chars[a].is_whitespace() {
            a += 1;
        }
        while b > a && chars[b - 1].is_whitespace() {
            b -= 1;
        }
        if a < b {
            ranges.push((a, b));
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
            continue;
        }
        if matches!(c, '\'' | '"' | '`') {
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == '$' {
            // $tag$ ... $tag$ (tag may be empty); a lone `$1` parameter isn't one.
            let tag_end = (i + 1..chars.len()).find(|&j| !(chars[j].is_alphanumeric() || chars[j] == '_'));
            if let Some(e) = tag_end
                && chars[e] == '$'
                && !chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())
            {
                let tag: Vec<char> = chars[i..=e].to_vec();
                let mut j = e + 1;
                while j < chars.len() && chars[j..].iter().take(tag.len()).ne(tag.iter()) {
                    j += 1;
                }
                i = (j + tag.len()).min(chars.len());
                continue;
            }
        }
        if c == ';' {
            push(start, i, &mut ranges);
            start = i + 1;
        } else if blank_lines && c == '\n' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '\n' && chars[j].is_whitespace() {
                j += 1;
            }
            if chars.get(j) == Some(&'\n') {
                push(start, i, &mut ranges);
                start = j;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    push(start, chars.len(), &mut ranges);
    ranges
}

/// The statement the cursor (a char index) is in or right after; the next one when the cursor
/// is before the first.
#[cfg(test)]
pub fn statement_at(script: &str, cursor: usize) -> Option<(usize, usize)> {
    statement_in(&statement_ranges(script, true), cursor)
}

/// `statement_at` with the ranges already split (the editor keeps them between frames).
pub fn statement_in(ranges: &[(usize, usize)], cursor: usize) -> Option<(usize, usize)> {
    ranges
        .iter()
        .rev()
        .find(|(a, _)| *a <= cursor)
        .or(ranges.first())
        .copied()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TxControl {
    Begin,
    End,
}

/// Whether a script opens or closes a transaction itself (BEGIN, COMMIT, ROLLBACK...); the
/// last such statement wins. `ROLLBACK TO SAVEPOINT` doesn't end anything.
pub fn tx_control(script: &str) -> Option<TxControl> {
    statement_words(script)
        .iter()
        .filter_map(|w| match (w[0].as_str(), w.get(1).map(String::as_str)) {
            ("BEGIN" | "START", _) => Some(TxControl::Begin),
            ("ROLLBACK", Some("TO")) => None,
            ("COMMIT" | "END" | "ROLLBACK" | "ABORT", _) => Some(TxControl::End),
            _ => None,
        })
        .next_back()
}

/// The two queries behind "Estatísticas da coluna": totals (count, non-null, distinct and, with
/// `ordered`, min/max), then the most common values. Postgres compares through text so json and
/// other types without equality still work.
pub fn column_stats(
    kind: Kind,
    schema: &str,
    table: &str,
    column: &str,
    filter: &str,
    ordered: bool,
) -> (String, String) {
    let t = kind.qualified(schema, table);
    let c = kind.quote_ident(column);
    let as_text = match kind {
        Kind::Postgres => format!("{c}::text"),
        Kind::MySql => c.clone(),
        Kind::Sqlite => format!("CAST({c} AS TEXT)"),
    };
    let wh = if filter.trim().is_empty() {
        String::new()
    } else {
        format!(" WHERE ({filter})")
    };
    let range = if ordered {
        format!(", min({c}), max({c})")
    } else {
        String::new()
    };
    (
        format!("SELECT count(*), count({c}), count(DISTINCT {as_text}){range} FROM {t}{wh}"),
        format!(
            "SELECT {as_text}, count(*) FROM {t}{wh} GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT {}",
            crate::values::TOP_VALUES
        ),
    )
}

/// Named parameters (`:id`, `:nome`) in a script: char ranges of each occurrence (colon
/// included) with the name. Not parameters: `::` casts, MySQL's `:=`, `a[1:2]` slices, and
/// anything inside quotes, comments or `$$` bodies.
pub fn param_spans(script: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<char> = script.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
            continue;
        }
        if matches!(c, '\'' | '"' | '`') {
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == '$' {
            let tag_end = (i + 1..chars.len()).find(|&j| !(chars[j].is_alphanumeric() || chars[j] == '_'));
            if let Some(e) = tag_end
                && chars[e] == '$'
                && !chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())
            {
                let tag: Vec<char> = chars[i..=e].to_vec();
                let mut j = e + 1;
                while j < chars.len() && chars[j..].iter().take(tag.len()).ne(tag.iter()) {
                    j += 1;
                }
                i = (j + tag.len()).min(chars.len());
                continue;
            }
        }
        if c == ':' {
            if chars.get(i + 1) == Some(&':') {
                i += 2;
                continue;
            }
            let prev_word =
                i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_' || chars[i - 1] == ']');
            let starts_name = chars.get(i + 1).is_some_and(|n| n.is_alphabetic() || *n == '_');
            if starts_name && !prev_word {
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                out.push((i, j, chars[i + 1..j].iter().collect()));
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Distinct parameter names in order of first use.
pub fn param_names(script: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (_, _, n) in param_spans(script) {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names
}

/// Puts the values in: numbers and NULL as typed, everything else as a quoted string.
pub fn bind_params(kind: Kind, script: &str, value: impl Fn(&str) -> String) -> String {
    let chars: Vec<char> = script.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    for (a, b, name) in param_spans(script) {
        out.extend(&chars[at..a]);
        let v = value(&name);
        let t = v.trim();
        let number = !t.is_empty()
            && t.trim_start_matches('-')
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.')
            && t.chars().filter(|c| *c == '.').count() <= 1
            && t.chars().any(|c| c.is_ascii_digit());
        if t.eq_ignore_ascii_case("null") {
            out.push_str("NULL");
        } else if number {
            out.push_str(t);
        } else {
            out.push_str(&kind.quote_str(&v));
        }
        at = b;
    }
    out.extend(&chars[at..]);
    out
}

/// Char range of the identifier at `cursor` (letters, digits, `_`, `$`, dots and quotes, so
/// `public."Tabela"` comes whole). `None` on whitespace or punctuation.
pub fn word_at(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '.' | '"' | '`');
    // The pointer sits between chars: take the one after it, or the one before at a word's end.
    let at = if chars.get(cursor).is_some_and(|c| is_word(*c)) {
        cursor
    } else if cursor > 0 && chars.get(cursor - 1).is_some_and(|c| is_word(*c)) {
        cursor - 1
    } else {
        return None;
    };
    let mut a = at;
    while a > 0 && is_word(chars[a - 1]) {
        a -= 1;
    }
    let mut b = at + 1;
    while b < chars.len() && is_word(chars[b]) {
        b += 1;
    }
    // Trailing dots ("tabela.") aren't part of the name.
    while b > a && chars[b - 1] == '.' {
        b -= 1;
    }
    (a < b).then_some((a, b))
}

/// One argument of a function or procedure, from its signature text.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutineArg {
    /// IN, OUT, INOUT or VARIADIC.
    pub mode: String,
    pub name: String,
    pub data_type: String,
}

/// `a integer, OUT total numeric, numeric(10,2)` (Postgres) or `IN pid int, IN qtd int` (MySQL)
/// into its arguments. Unnamed ones are called `p1`, `p2`…
pub fn parse_args(args: &str) -> Vec<RoutineArg> {
    let mut parts = Vec::new();
    let (mut depth, mut cur) = (0, String::new());
    for c in args.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    parts.push(cur);
    parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .enumerate()
        .map(|(i, p)| {
            let mut words: Vec<&str> = p.split_whitespace().collect();
            let mode = match words.first().map(|w| w.to_uppercase()) {
                Some(m) if matches!(m.as_str(), "IN" | "OUT" | "INOUT" | "VARIADIC") => {
                    words.remove(0);
                    m
                }
                _ => "IN".to_string(),
            };
            // "name type…" when there are two or more words and the first isn't a type that
            // takes several words ("double precision", "character varying").
            let multi_word_type = matches!(
                words.first().map(|w| w.to_lowercase()).as_deref(),
                Some("double" | "character" | "timestamp" | "time" | "bit" | "interval")
            );
            let (name, data_type) = if words.len() >= 2 && !multi_word_type {
                (words[0].trim_matches('"').to_string(), words[1..].join(" "))
            } else {
                (format!("p{}", i + 1), words.join(" "))
            };
            RoutineArg {
                mode,
                name,
                data_type,
            }
        })
        .collect()
}

/// The call of a function or procedure with `:parâmetros` for its inputs. OUT arguments are
/// NULL placeholders (Postgres) or session variables read back afterwards (MySQL).
pub fn call_template(kind: Kind, schema: &str, name: &str, args: &[RoutineArg], procedure: bool) -> String {
    let target = kind.qualified(schema, name);
    let mut outs = Vec::new();
    let values: Vec<String> = args
        .iter()
        .map(|a| match (a.mode.as_str(), kind) {
            ("OUT", Kind::Postgres) => "NULL".to_string(),
            ("OUT" | "INOUT", Kind::MySql) => {
                let var = format!("@{}", a.name);
                outs.push(var.clone());
                var
            }
            _ => {
                let p: String = a
                    .name
                    .chars()
                    .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
                    .collect();
                format!(":{p}")
            }
        })
        .collect();
    let call = format!("{target}({})", values.join(", "));
    match (procedure, kind) {
        (false, Kind::Postgres) => format!("SELECT * FROM {call};"),
        (false, Kind::MySql | Kind::Sqlite) => format!("SELECT {call} AS resultado;"),
        (true, _) if outs.is_empty() => format!("CALL {call};"),
        (true, _) => format!("CALL {call};\nSELECT {};", outs.join(", ")),
    }
}

/// Table actions from the sidebar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TableAction {
    Rename,
    /// A copy with the same columns, indexes and defaults; with the rows too when `true`.
    Duplicate(bool),
    Truncate,
    Drop,
}

/// The statements for a table action (`name`: the new name for rename / duplicate).
pub fn table_action(kind: Kind, schema: &str, table: &str, action: TableAction, name: &str) -> String {
    let t = kind.qualified(schema, table);
    let new = kind.qualified(schema, name.trim());
    match (action, kind) {
        (TableAction::Rename, Kind::Postgres) => {
            format!("ALTER TABLE {t} RENAME TO {};", kind.quote_ident(name.trim()))
        }
        (TableAction::Rename, Kind::MySql) => format!("RENAME TABLE {t} TO {new};"),
        (TableAction::Duplicate(data), Kind::Postgres) => {
            let mut s = format!("CREATE TABLE {new} (LIKE {t} INCLUDING ALL);");
            if data {
                // Identity columns refuse given values unless told otherwise.
                s.push_str(&format!(
                    "\nINSERT INTO {new} OVERRIDING SYSTEM VALUE SELECT * FROM {t};"
                ));
            }
            s
        }
        (TableAction::Duplicate(data), Kind::MySql) => {
            let mut s = format!("CREATE TABLE {new} LIKE {t};");
            if data {
                s.push_str(&format!("\nINSERT INTO {new} SELECT * FROM {t};"));
            }
            s
        }
        (TableAction::Rename, Kind::Sqlite) => {
            format!("ALTER TABLE {t} RENAME TO {};", kind.quote_ident(name.trim()))
        }
        // No CREATE TABLE … LIKE in SQLite: the columns (and rows) come, the rest doesn't.
        (TableAction::Duplicate(data), Kind::Sqlite) => format!(
            "-- SQLite: a cópia leva as colunas{}, mas não a chave primária, os índices nem as restrições.\n\
             CREATE TABLE {new} AS SELECT * FROM {t}{};",
            if data { " e os dados" } else { "" },
            if data { "" } else { " WHERE 0" }
        ),
        (TableAction::Truncate, Kind::Sqlite) => format!("DELETE FROM {t};"),
        (TableAction::Truncate, _) => format!("TRUNCATE TABLE {t};"),
        (TableAction::Drop, _) => format!("DROP TABLE {t};"),
    }
}

/// What "Gerar SQL" writes for a table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Template {
    Select,
    Insert,
    Update,
    Delete,
}

/// A statement for a table, with `:parâmetros` where values go (the editor asks for them when
/// it runs). `cols` are `(name, is primary key)`; without a key the WHERE uses every column.
pub fn template(kind: Kind, schema: &str, table: &str, cols: &[(String, bool)], what: Template) -> String {
    let t = kind.qualified(schema, table);
    let q = |c: &str| kind.quote_ident(c);
    // Parameter names are plain words: anything else in a column name becomes "_".
    let p = |c: &str| {
        let s: String = c
            .chars()
            .map(|ch| {
                if ch.is_alphanumeric() || ch == '_' {
                    ch
                } else {
                    '_'
                }
            })
            .collect();
        format!(":{s}")
    };
    let keys: Vec<&String> = {
        let pk: Vec<&String> = cols.iter().filter(|(_, k)| *k).map(|(c, _)| c).collect();
        if pk.is_empty() {
            cols.iter().map(|(c, _)| c).collect()
        } else {
            pk
        }
    };
    let cond = keys
        .iter()
        .map(|c| format!("{} = {}", q(c), p(c)))
        .collect::<Vec<_>>()
        .join("\n  AND ");
    let names = cols.iter().map(|(c, _)| q(c)).collect::<Vec<_>>();
    match what {
        Template::Select => format!(
            "SELECT {}\nFROM {t}\nWHERE 1 = 1\nLIMIT 100;",
            names.join(",\n       ")
        ),
        Template::Insert => format!(
            "INSERT INTO {t} ({})\nVALUES ({});",
            names.join(", "),
            cols.iter().map(|(c, _)| p(c)).collect::<Vec<_>>().join(", ")
        ),
        Template::Update => {
            let sets: Vec<String> = cols
                .iter()
                .filter(|(c, _)| !keys.contains(&c))
                .map(|(c, _)| format!("{} = {}", q(c), p(c)))
                .collect();
            format!("UPDATE {t}\nSET {}\nWHERE {cond};", sets.join(",\n    "))
        }
        Template::Delete => format!("DELETE FROM {t}\nWHERE {cond};"),
    }
}

/// Words outside parentheses, quotes and comments, uppercased, with their char ranges.
fn top_level_words(script: &str) -> Vec<(String, usize, usize)> {
    let chars: Vec<char> = script.chars().collect();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
            continue;
        }
        if matches!(c, '\'' | '"' | '`') {
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            if depth == 0 {
                out.push((
                    chars[start..i].iter().collect::<String>().to_uppercase(),
                    start,
                    i,
                ));
            }
            continue;
        }
        i += 1;
    }
    out
}

/// `(schema, table)` of a SELECT whose rows map one-to-one onto a table's rows: one table in
/// FROM (an alias is fine), no joins, grouping, DISTINCT, set operations or CTEs. Names come
/// unquoted.
pub fn editable_select(statement: &str) -> Option<(Option<String>, String)> {
    if statement_ranges(statement, false).len() != 1 {
        return None;
    }
    let sql = statement.trim().trim_end_matches(';').trim_end();
    let chars: Vec<char> = sql.chars().collect();
    let words = top_level_words(sql);
    if words.first()?.0 != "SELECT" {
        return None;
    }
    const BLOCKERS: &[&str] = &[
        "JOIN",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        "GROUP",
        "HAVING",
        "DISTINCT",
        "INTO",
        "WINDOW",
        "LATERAL",
    ];
    if words.iter().any(|(w, _, _)| BLOCKERS.contains(&w.as_str())) {
        return None;
    }
    let from = words.iter().position(|(w, _, _)| w == "FROM")?;
    let end = words[from + 1..]
        .iter()
        .find(|(w, _, _)| {
            matches!(
                w.as_str(),
                "WHERE" | "ORDER" | "LIMIT" | "OFFSET" | "FETCH" | "FOR"
            )
        })
        .map_or(chars.len(), |(_, a, _)| *a);
    let target: String = chars[words[from].2..end].iter().collect();
    let target = target.trim();
    if target.is_empty() || target.contains([',', '(']) {
        return None;
    }
    // The name is the first token; what follows can only be an alias.
    let mut name = String::new();
    let mut quote = None;
    for c in target.chars() {
        match (quote, c) {
            (None, '"' | '`') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => break,
            _ => {}
        }
        name.push(c);
    }
    let rest: Vec<&str> = target[name.len()..].split_whitespace().collect();
    match rest[..] {
        [] | [_] => {}
        [a, _] if a.eq_ignore_ascii_case("AS") => {}
        _ => return None,
    }
    let unquote = |p: &str| p.trim_matches(|c| c == '"' || c == '`').to_string();
    let parts = split_dotted(&name);
    match parts.len() {
        1 => Some((None, unquote(&parts[0]))),
        2 => Some((Some(unquote(&parts[0])), unquote(&parts[1]))),
        _ => None,
    }
}

/// `a.b`, `"a.x"."b"`: splits on dots outside quotes.
fn split_dotted(name: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut quote = None;
    for c in name.chars() {
        match (quote, c) {
            (None, '"' | '`') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '.') => {
                parts.push(String::new());
                continue;
            }
            _ => {}
        }
        parts.last_mut().unwrap().push(c);
    }
    parts
}

/// Reads for "prévia" of an UPDATE or DELETE, without running it: how many rows it would
/// touch, and those rows as they are now.
#[derive(Debug, PartialEq)]
pub struct PreviewQueries {
    pub count: String,
    pub sample: String,
}

/// Rows shown in the preview.
pub const PREVIEW_SAMPLE: usize = 50;

/// `UPDATE t [alias] SET … [WHERE …]` and `DELETE FROM t [alias] [WHERE …]` become a count and
/// a SELECT of the same rows. `None` for forms where that rewrite would be wrong (joins,
/// `UPDATE … FROM`, `DELETE … USING`, multi-table MySQL statements).
pub fn preview_queries(statement: &str) -> Option<PreviewQueries> {
    let sql = statement.trim().trim_end_matches(';').trim_end();
    let chars: Vec<char> = sql.chars().collect();
    let text = |a: usize, b: usize| chars[a..b].iter().collect::<String>().trim().to_string();
    let words = top_level_words(sql);
    let find = |from: usize, w: &[&str]| {
        words
            .iter()
            .skip(from)
            .position(|(x, _, _)| w.contains(&x.as_str()))
            .map(|p| p + from)
    };
    let first = words.first()?.0.as_str();
    let (table, where_at) = match first {
        "UPDATE" => {
            let set = find(1, &["SET"])?;
            // Skip MySQL modifiers and Postgres ONLY.
            let mut t0 = 1;
            while t0 < set && matches!(words[t0].0.as_str(), "LOW_PRIORITY" | "IGNORE" | "ONLY") {
                t0 += 1;
            }
            let table = text(words.get(t0)?.1, words[set].1);
            if table.contains(',') || words[t0..set].iter().any(|(w, _, _)| w == "JOIN") {
                return None;
            }
            if find(set, &["FROM"]).is_some() {
                return None;
            }
            (table, find(set, &["WHERE"]))
        }
        "DELETE" => {
            if words.get(1)?.0 != "FROM" {
                return None;
            }
            let mut t0 = 2;
            while matches!(words.get(t0).map(|w| w.0.as_str()), Some("ONLY")) {
                t0 += 1;
            }
            let stop = find(t0, &["WHERE", "USING", "ORDER", "LIMIT", "RETURNING"]);
            let end = stop.map_or(chars.len(), |i| words[i].1);
            let table = text(words.get(t0)?.1, end);
            if stop.is_some_and(|i| words[i].0 == "USING") || table.contains(',') {
                return None;
            }
            (table, stop.filter(|&i| words[i].0 == "WHERE"))
        }
        _ => return None,
    };
    let condition = match where_at {
        Some(w) => {
            let end = find(w + 1, &["ORDER", "LIMIT", "RETURNING"]).map_or(chars.len(), |i| words[i].1);
            format!(" {}", text(words[w].1, end))
        }
        None => String::new(),
    };
    Some(PreviewQueries {
        count: format!("SELECT count(*) FROM {table}{condition}"),
        sample: format!("SELECT * FROM {table}{condition} LIMIT {PREVIEW_SAMPLE}"),
    })
}

/// MySQL commits the open transaction by itself before (and after) DDL statements.
pub fn mysql_implicit_commit(script: &str) -> bool {
    const DDL: &[&str] = &[
        "CREATE", "ALTER", "DROP", "TRUNCATE", "RENAME", "GRANT", "REVOKE", "LOCK", "UNLOCK",
    ];
    statement_words(script)
        .iter()
        .any(|w| DDL.contains(&w[0].as_str()))
}

/// The first word of the first statement, uppercased.
pub fn statement_words_first(script: &str) -> Option<String> {
    statement_words(script).first()?.first().cloned()
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
            Kind::MySql | Kind::Sqlite => sqlformat::Dialect::Generic,
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
        Kind::Postgres | Kind::Sqlite => format!("INSERT INTO {t} DEFAULT VALUES"),
        Kind::MySql => format!("INSERT INTO {t} () VALUES ()"),
    })
}

pub fn delete_row(kind: Kind, schema: &str, table: &str, wh: Stmt) -> Stmt {
    Stmt::new(format!("DELETE FROM {} WHERE ", kind.qualified(schema, table))).append(wh)
}

/// Values "Filtrar valores" lists at most (the most common ones).
pub const DISTINCT_LIMIT: usize = 1000;

/// The column as "Filtrar valores" compares it: Postgres `json`/`xml` have no equality, so as text.
pub fn value_expr(kind: Kind, column: &str, data_type: &str) -> String {
    let c = kind.quote_ident(column);
    match kind {
        Kind::Postgres if matches!(data_type, "json" | "xml") => format!("{c}::text"),
        _ => c,
    }
}

/// Distinct values of `expr` and how many rows have each, most common first; one more than
/// `DISTINCT_LIMIT` to tell whether the list is complete. `search` narrows it on the server.
pub fn distinct_values(
    kind: Kind,
    schema: &str,
    table: &str,
    expr: &str,
    filter: &str,
    search: &str,
) -> String {
    let search = search.trim();
    let cond = if search.is_empty() {
        filter.to_string()
    } else {
        and(filter, &contains(kind, expr, search))
    };
    let wh = if cond.trim().is_empty() {
        String::new()
    } else {
        format!(" WHERE {cond}")
    };
    format!(
        "SELECT {expr}, count(*) FROM {}{wh} GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT {}",
        kind.qualified(schema, table),
        DISTINCT_LIMIT + 1
    )
}

/// The condition keeping only the checked values (`None`: everything is checked). With the
/// whole list known, the unchecked ones are listed instead when that is shorter. NULL is
/// handled apart: `IN`/`NOT IN` never match it.
pub fn values_condition(
    kind: Kind,
    expr: &str,
    checked: &[Option<String>],
    unchecked: &[Option<String>],
    complete: bool,
) -> Option<String> {
    if unchecked.is_empty() {
        return None;
    }
    let list = |vals: &[Option<String>]| {
        vals.iter()
            .flatten()
            .map(|v| kind.quote_str(v))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let has_null = |vals: &[Option<String>]| vals.iter().any(Option::is_none);
    if complete && unchecked.len() < checked.len() {
        let out = list(unchecked);
        return Some(match (out.is_empty(), has_null(checked)) {
            (true, _) => format!("{expr} IS NOT NULL"),
            (false, true) => format!("({expr} NOT IN ({out}) OR {expr} IS NULL)"),
            (false, false) => format!("{expr} NOT IN ({out})"),
        });
    }
    let keep = list(checked);
    Some(match (keep.is_empty(), has_null(checked)) {
        (true, true) => format!("{expr} IS NULL"),
        // Nothing checked: no row.
        (true, false) => "1 = 0".to_string(),
        (false, true) => format!("({expr} IN ({keep}) OR {expr} IS NULL)"),
        (false, false) => format!("{expr} IN ({keep})"),
    })
}

/// `filter` without `cond`, when `cond` was added with `and` (as the whole filter, its last
/// part or its first). `None` when it isn't there any more (the user edited the filter).
pub fn remove_condition(filter: &str, cond: &str) -> Option<String> {
    let (f, c) = (filter.trim(), cond.trim());
    if f == c {
        return Some(String::new());
    }
    if let Some(rest) = f.strip_suffix(&format!(" AND {c}")) {
        return Some(unwrap_parens(rest).to_string());
    }
    // It was first, and something was added after it: "(cond) AND rest".
    f.strip_prefix(&format!("({c}) AND "))
        .map(|rest| rest.trim().to_string())
}

/// `(a)` → `a`, but `(a) AND (b)` stays.
fn unwrap_parens(s: &str) -> &str {
    let s = s.trim();
    let Some(inner) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')) else {
        return s;
    };
    let mut depth = 0i32;
    for c in inner.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return s;
        }
    }
    inner
}

/// Referenced tables up to this many rows are loaded whole for the foreign-key picker and
/// filtered as you type; bigger ones are searched in the database.
pub const FK_LOCAL_ROWS: i64 = 10_000;
/// Rows a database search of the picker brings.
pub const FK_SEARCH_ROWS: usize = 50;

/// The rows the foreign-key picker lists: `(key, label)` of the referenced table. `search`
/// empty: the start of the table (all of it when `local`). A number typed matches the key
/// exactly (instant, by the primary key index); text is looked for inside the label.
pub fn fk_options(
    kind: Kind,
    fk: &crate::db::FkRef,
    label: Option<&str>,
    numeric_key: bool,
    local: bool,
    search: &str,
) -> String {
    let key = kind.quote_ident(&fk.column);
    let shown = label.map(|l| kind.quote_ident(l));
    let cols = match &shown {
        Some(l) => format!("{key}, {l}"),
        None => format!("{key}, NULL"),
    };
    // MySQL stops a SELECT by itself after 2 s with this hint (Postgres: see the worker).
    let select = match kind {
        Kind::MySql => format!("SELECT /*+ MAX_EXECUTION_TIME(2000) */ {cols}"),
        _ => format!("SELECT {cols}"),
    };
    let table = kind.qualified(&fk.schema, &fk.table);
    if local {
        let order = shown.as_ref().unwrap_or(&key);
        return format!(
            "{select} FROM {table} ORDER BY {order} LIMIT {}",
            FK_LOCAL_ROWS + 1
        );
    }
    let term = search.trim();
    if term.is_empty() {
        return format!("{select} FROM {table} LIMIT {FK_SEARCH_ROWS}");
    }
    let mut conds = vec![contains(kind, shown.as_ref().unwrap_or(&key), term)];
    if numeric_key && term.parse::<i64>().is_ok() {
        conds.push(format!("{key} = {term}"));
    } else if !numeric_key {
        conds.push(format!("{key} = {}", kind.quote_str(term)));
    }
    // No ORDER BY: the search stops at the first matches instead of sorting all of them.
    format!(
        "{select} FROM {table} WHERE {} LIMIT {FK_SEARCH_ROWS}",
        conds.join(" OR ")
    )
}

/// The referenced table's column worth showing beside the key: a name, title, e-mail… or else
/// its first text column.
pub fn fk_label(columns: &[(String, String)], key: &str) -> Option<String> {
    const NAMES: &[&str] = &[
        "nome",
        "name",
        "razao_social",
        "nome_fantasia",
        "titulo",
        "title",
        "descricao",
        "description",
        "email",
        "login",
        "username",
        "usuario",
        "sku",
        "codigo",
        "code",
        "label",
        "slug",
    ];
    let lower = |s: &str| s.to_lowercase();
    NAMES
        .iter()
        .find_map(|n| columns.iter().find(|(c, _)| lower(c) == *n && c != key))
        .or_else(|| {
            columns.iter().find(|(c, t)| {
                let t = lower(t);
                c != key && (t.contains("char") || t.contains("text"))
            })
        })
        .map(|(c, _)| c.clone())
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
    fn column_stats_queries() {
        let (sum, top) = column_stats(Kind::Postgres, "public", "t", "pre ço", "a > 1", true);
        assert_eq!(
            sum,
            "SELECT count(*), count(\"pre ço\"), count(DISTINCT \"pre ço\"::text), min(\"pre ço\"), max(\"pre ço\") FROM \"public\".\"t\" WHERE (a > 1)"
        );
        assert_eq!(
            top,
            "SELECT \"pre ço\"::text, count(*) FROM \"public\".\"t\" WHERE (a > 1) GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT 10"
        );
        let (sum, _) = column_stats(Kind::MySql, "db", "t", "c", "", false);
        assert_eq!(
            sum,
            "SELECT count(*), count(`c`), count(DISTINCT `c`) FROM `db`.`t`"
        );
    }

    #[test]
    fn named_parameters() {
        let q = "SELECT x::text, ':nao' AS s, a[1:2], @v := 1 -- :tambem_nao\nFROM t WHERE id = :id AND (nome = :nome OR :id IS NULL)";
        assert_eq!(param_names(q), ["id", "nome"]);
        let bound = bind_params(Kind::Postgres, q, |n| {
            if n == "id" { "42".into() } else { "O'Neil".into() }
        });
        assert!(
            bound.ends_with("WHERE id = 42 AND (nome = 'O''Neil' OR 42 IS NULL)"),
            "{bound}"
        );
        assert!(bound.starts_with("SELECT x::text, ':nao'"));
        assert_eq!(bind_params(Kind::MySql, "a = :v", |_| "null".into()), "a = NULL");
        assert_eq!(bind_params(Kind::MySql, "a = :v", |_| "-1.5".into()), "a = -1.5");
        assert_eq!(
            bind_params(Kind::MySql, "a = :v", |_| "1.2.3".into()),
            "a = '1.2.3'"
        );
        assert!(param_names("SELECT $$ :x $$, '10:30'").is_empty());
    }

    #[test]
    fn table_actions() {
        let a = |k, w, n: &str| table_action(k, "s", "t", w, n);
        assert_eq!(
            a(Kind::Postgres, TableAction::Rename, "novo"),
            "ALTER TABLE \"s\".\"t\" RENAME TO \"novo\";"
        );
        assert_eq!(
            a(Kind::MySql, TableAction::Rename, "novo"),
            "RENAME TABLE `s`.`t` TO `s`.`novo`;"
        );
        assert_eq!(
            a(Kind::Postgres, TableAction::Duplicate(true), "copia"),
            "CREATE TABLE \"s\".\"copia\" (LIKE \"s\".\"t\" INCLUDING ALL);\nINSERT INTO \"s\".\"copia\" OVERRIDING SYSTEM VALUE SELECT * FROM \"s\".\"t\";"
        );
        assert_eq!(
            a(Kind::MySql, TableAction::Duplicate(false), "copia"),
            "CREATE TABLE `s`.`copia` LIKE `s`.`t`;"
        );
        assert_eq!(a(Kind::MySql, TableAction::Drop, ""), "DROP TABLE `s`.`t`;");
    }

    #[test]
    fn calls_routines_with_parameters() {
        let a = parse_args("a integer, OUT total numeric, numeric(10,2), double precision");
        assert_eq!(
            a.iter()
                .map(|x| (x.mode.as_str(), x.name.as_str(), x.data_type.as_str()))
                .collect::<Vec<_>>(),
            [
                ("IN", "a", "integer"),
                ("OUT", "total", "numeric"),
                ("IN", "p3", "numeric(10,2)"),
                ("IN", "p4", "double precision"),
            ]
        );
        assert_eq!(
            call_template(Kind::Postgres, "public", "f", &parse_args("cid integer"), false),
            "SELECT * FROM \"public\".\"f\"(:cid);"
        );
        assert_eq!(
            call_template(Kind::Postgres, "public", "p", &a, true),
            "CALL \"public\".\"p\"(:a, NULL, :p3, :p4);"
        );
        let my = parse_args("IN pid int, OUT novo int");
        assert_eq!(
            call_template(Kind::MySql, "loja", "repor", &my, true),
            "CALL `loja`.`repor`(:pid, @novo);\nSELECT @novo;"
        );
        assert_eq!(
            call_template(Kind::MySql, "loja", "f", &[], false),
            "SELECT `loja`.`f`() AS resultado;"
        );
        assert_eq!(
            param_names(&call_template(Kind::Postgres, "public", "p", &a, true)),
            ["a", "p3", "p4"]
        );
    }

    #[test]
    fn templates_for_a_table() {
        let cols = [
            ("id".to_string(), true),
            ("nome".to_string(), false),
            ("data nasc".to_string(), false),
        ];
        let t = |w| template(Kind::Postgres, "public", "clientes", &cols, w);
        assert_eq!(
            t(Template::Update),
            "UPDATE \"public\".\"clientes\"\nSET \"nome\" = :nome,\n    \"data nasc\" = :data_nasc\nWHERE \"id\" = :id;"
        );
        assert_eq!(
            t(Template::Delete),
            "DELETE FROM \"public\".\"clientes\"\nWHERE \"id\" = :id;"
        );
        assert_eq!(
            t(Template::Insert),
            "INSERT INTO \"public\".\"clientes\" (\"id\", \"nome\", \"data nasc\")\nVALUES (:id, :nome, :data_nasc);"
        );
        assert!(t(Template::Select).starts_with("SELECT \"id\",\n       \"nome\""));
        // The parameters are the ones the editor will ask for.
        assert_eq!(param_names(&t(Template::Update)), ["nome", "data_nasc", "id"]);
        // No key: every column identifies the row.
        let nokey = [("a".to_string(), false), ("b".to_string(), false)];
        assert_eq!(
            template(Kind::MySql, "db", "t", &nokey, Template::Delete),
            "DELETE FROM `db`.`t`\nWHERE `a` = :a\n  AND `b` = :b;"
        );
    }

    #[test]
    fn words_under_the_pointer() {
        let text = "SELECT * FROM public.\"Clientes\" c WHERE c.id = 1";
        let w =
            |i: usize| word_at(text, i).map(|(a, b)| text.chars().skip(a).take(b - a).collect::<String>());
        assert_eq!(w(16).as_deref(), Some("public.\"Clientes\""));
        assert_eq!(
            w(31).as_deref(),
            Some("public.\"Clientes\""),
            "right after the word"
        );
        assert_eq!(w(7), None, "on punctuation");
        assert_eq!(w(0).as_deref(), Some("SELECT"));
    }

    #[test]
    fn finds_the_table_of_editable_selects() {
        let t = |s: &str| editable_select(s);
        assert_eq!(
            t("SELECT * FROM clientes WHERE uf = 'SP' ORDER BY id LIMIT 10;"),
            Some((None, "clientes".into()))
        );
        assert_eq!(
            t("select id, nome from public.clientes c where c.ativo"),
            Some((Some("public".into()), "clientes".into()))
        );
        assert_eq!(
            t("SELECT * FROM \"Meu Esquema\".\"Tabela.X\" AS t"),
            Some((Some("Meu Esquema".into()), "Tabela.X".into()))
        );
        assert_eq!(
            t("SELECT * FROM `loja`.`produtos`"),
            Some((Some("loja".into()), "produtos".into()))
        );
        // A subquery in WHERE is fine; it isn't in FROM.
        assert!(
            t("SELECT * FROM pedidos WHERE cliente_id IN (SELECT id FROM clientes JOIN x ON true)").is_some()
        );
        for no in [
            "SELECT * FROM a JOIN b ON a.id = b.id",
            "SELECT * FROM a, b",
            "SELECT uf, count(*) FROM clientes GROUP BY uf",
            "SELECT DISTINCT uf FROM clientes",
            "SELECT * FROM (SELECT 1) x",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "SELECT 1 UNION SELECT 2",
            "SELECT 1",
            "SELECT * FROM a; SELECT * FROM b",
            "UPDATE t SET a = 1",
        ] {
            assert_eq!(t(no), None, "{no}");
        }
    }

    #[test]
    fn previews_rewrite_writes_as_reads() {
        let p = preview_queries("UPDATE clientes c SET saldo = (SELECT 1 WHERE x) WHERE c.uf = 'SP' AND (a OR b) ORDER BY id LIMIT 10;").unwrap();
        assert_eq!(
            p.count,
            "SELECT count(*) FROM clientes c WHERE c.uf = 'SP' AND (a OR b)"
        );
        assert_eq!(
            p.sample,
            "SELECT * FROM clientes c WHERE c.uf = 'SP' AND (a OR b) LIMIT 50"
        );
        let p = preview_queries("delete from public.pedidos where status = 'where' returning id").unwrap();
        assert_eq!(
            p.count,
            "SELECT count(*) FROM public.pedidos where status = 'where'"
        );
        // Everything: no WHERE.
        assert_eq!(
            preview_queries("UPDATE LOW_PRIORITY t SET a = 1").unwrap().count,
            "SELECT count(*) FROM t"
        );
        assert_eq!(
            preview_queries("DELETE FROM t").unwrap().sample,
            "SELECT * FROM t LIMIT 50"
        );
        // Forms the rewrite can't express.
        assert!(preview_queries("UPDATE t SET a = o.a FROM o WHERE o.id = t.id").is_none());
        assert!(preview_queries("UPDATE t JOIN o ON o.id = t.id SET t.a = 1").is_none());
        assert!(preview_queries("UPDATE t, o SET t.a = 1").is_none());
        assert!(preview_queries("DELETE FROM t USING o WHERE o.id = t.id").is_none());
        assert!(preview_queries("DELETE t FROM t JOIN o ON o.id = t.id").is_none());
        assert!(preview_queries("SELECT 1").is_none());
    }

    #[test]
    fn transaction_control() {
        assert_eq!(tx_control("UPDATE t SET a = 1"), None);
        assert_eq!(tx_control("begin; update t set a = 1"), Some(TxControl::Begin));
        assert_eq!(
            tx_control("BEGIN; UPDATE t SET a = 1; COMMIT;"),
            Some(TxControl::End)
        );
        assert_eq!(tx_control("START TRANSACTION"), Some(TxControl::Begin));
        assert_eq!(tx_control("ROLLBACK TO SAVEPOINT x"), None);
        assert_eq!(tx_control("SELECT 'commit'"), None);
        assert!(mysql_implicit_commit(
            "UPDATE t SET a = 1; ALTER TABLE t ADD b INT"
        ));
        assert!(!mysql_implicit_commit("UPDATE t SET a = 'drop'"));
    }

    #[test]
    fn statements_under_the_cursor() {
        let text = |s: &str, r: Option<(usize, usize)>| {
            r.map(|(a, b)| s.chars().skip(a).take(b - a).collect::<String>())
        };
        let script = "SELECT 1;\nSELECT ';' -- ;\n  , 2;\n\nUPDATE t SET a = $$x;y$$ WHERE id = $1";
        let parts: Vec<String> = statement_ranges(script, false)
            .into_iter()
            .map(|r| text(script, Some(r)).unwrap())
            .collect();
        assert_eq!(
            parts,
            [
                "SELECT 1",
                "SELECT ';' -- ;\n  , 2",
                "UPDATE t SET a = $$x;y$$ WHERE id = $1"
            ]
        );
        // Cursor inside, right after the `;`, and on the blank line after a statement.
        assert_eq!(text(script, statement_at(script, 3)).as_deref(), Some("SELECT 1"));
        assert_eq!(text(script, statement_at(script, 9)).as_deref(), Some("SELECT 1"));
        let upd = script.chars().count() - 3;
        assert_eq!(
            text(script, statement_at(script, upd)).as_deref(),
            Some("UPDATE t SET a = $$x;y$$ WHERE id = $1")
        );
        // Without semicolons, blank lines separate statements.
        let scratch = "SELECT a\nFROM t\n\n  \nSELECT b";
        assert_eq!(
            text(scratch, statement_at(scratch, 2)).as_deref(),
            Some("SELECT a\nFROM t")
        );
        assert_eq!(
            text(scratch, statement_at(scratch, 20)).as_deref(),
            Some("SELECT b")
        );
        assert_eq!(statement_at("  ", 1), None);
        assert_eq!(
            text("x $tag$ a;b $tag$ y", statement_at("x $tag$ a;b $tag$ y", 0)).as_deref(),
            Some("x $tag$ a;b $tag$ y")
        );
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

    #[test]
    fn value_filters() {
        let k = Kind::Postgres;
        let v = |x: &str| Some(x.to_string());
        let sql = distinct_values(k, "public", "clientes", "\"uf\"", "ativo", "s");
        assert_eq!(
            sql,
            "SELECT \"uf\", count(*) FROM \"public\".\"clientes\" WHERE (ativo) AND CAST(\"uf\" AS TEXT) ILIKE '%s%' \
             GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT 1001"
        );
        let c = "\"uf\"";
        assert_eq!(values_condition(k, c, &[v("SP"), v("RJ")], &[], true), None);
        assert_eq!(
            values_condition(k, c, &[v("SP")], &[v("RJ"), None], true).as_deref(),
            Some("\"uf\" IN ('SP')")
        );
        // Fewer unchecked than checked: listed instead, NULL kept.
        assert_eq!(
            values_condition(k, c, &[v("SP"), v("RJ"), None], &[v("MG")], true).as_deref(),
            Some("(\"uf\" NOT IN ('MG') OR \"uf\" IS NULL)")
        );
        assert_eq!(
            values_condition(k, c, &[v("SP"), v("RJ")], &[None], true).as_deref(),
            Some("\"uf\" IS NOT NULL")
        );
        // Not every value was listed: only IN is safe.
        assert_eq!(
            values_condition(k, c, &[v("SP"), v("RJ"), None], &[v("MG")], false).as_deref(),
            Some("(\"uf\" IN ('SP', 'RJ') OR \"uf\" IS NULL)")
        );
        assert_eq!(
            values_condition(k, c, &[], &[v("SP")], true).as_deref(),
            Some("1 = 0")
        );

        let cond = "\"uf\" IN ('SP')";
        assert_eq!(remove_condition(cond, cond).as_deref(), Some(""));
        assert_eq!(
            remove_condition(&and("ativo", cond), cond).as_deref(),
            Some("ativo")
        );
        assert_eq!(
            remove_condition(&and("a OR b", cond), cond).as_deref(),
            Some("a OR b")
        );
        assert_eq!(remove_condition(&and(&and("(a) AND (b)", cond), "x"), cond), None);
        assert_eq!(
            remove_condition(&and(cond, "x > 1"), cond).as_deref(),
            Some("x > 1")
        );
        assert_eq!(unwrap_parens("(a) AND (b)"), "(a) AND (b)");
    }

    #[test]
    fn foreign_key_picker_queries() {
        let fk = crate::db::FkRef {
            schema: "public".into(),
            table: "clientes".into(),
            column: "id".into(),
        };
        let k = Kind::Postgres;
        assert_eq!(
            fk_options(k, &fk, Some("nome"), true, true, ""),
            "SELECT \"id\", \"nome\" FROM \"public\".\"clientes\" ORDER BY \"nome\" LIMIT 10001"
        );
        assert_eq!(
            fk_options(k, &fk, Some("nome"), true, false, "42"),
            "SELECT \"id\", \"nome\" FROM \"public\".\"clientes\" \
             WHERE CAST(\"nome\" AS TEXT) ILIKE '%42%' OR \"id\" = 42 LIMIT 50"
        );
        // Text that isn't a number never meets a numeric key (it would be an error).
        assert!(!fk_options(k, &fk, Some("nome"), true, false, "maria").contains("\"id\" ="));
        let my = fk_options(Kind::MySql, &fk, None, false, false, "a'b");
        assert!(
            my.starts_with(
                "SELECT /*+ MAX_EXECUTION_TIME(2000) */ `id`, NULL FROM `public`.`clientes` WHERE"
            ),
            "{my}"
        );
        assert!(my.contains("`id` = 'a''b'"), "{my}");

        let cols = |v: &[(&str, &str)]| {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            fk_label(
                &cols(&[("id", "integer"), ("email", "text"), ("Nome", "text")]),
                "id"
            )
            .as_deref(),
            Some("Nome")
        );
        assert_eq!(
            fk_label(
                &cols(&[("id", "integer"), ("total", "numeric"), ("obs", "varchar(20)")]),
                "id"
            )
            .as_deref(),
            Some("obs")
        );
        assert_eq!(
            fk_label(&cols(&[("id", "integer"), ("total", "numeric")]), "id"),
            None
        );
    }
}
