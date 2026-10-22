//! Builders for the statements the UI generates (table browsing and row edits).

use crate::dialect::Kind;
use std::fmt::Write as _;

/// Rows per page in table tabs.
pub fn page_size() -> usize {
    200
}

pub struct PageQuery<'a> {
    pub schema: &'a str,
    pub table: &'a str,
    pub filter: &'a str,
    /// Sort columns, the main one first; the primary key follows as the tiebreaker.
    pub order: &'a [(String, bool)],
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
        let _ = write!(sql, " WHERE ({})", q.filter);
    }
    // Without a stable order Postgres returns an updated row at the end of the table, so after
    // an edit the row would jump away and a neighbor would show up in its place.
    let mut keys: Vec<String> = Vec::new();
    for (col, asc) in q.order {
        keys.push(format!(
            "{} {}",
            kind.quote_ident(col),
            if *asc { "ASC" } else { "DESC" }
        ));
    }
    for p in q.pk {
        if !q.order.iter().any(|(c, _)| c == p) {
            keys.push(format!("{} ASC", kind.quote_ident(p)));
        }
    }
    if !keys.is_empty() {
        let _ = write!(sql, " ORDER BY {}", keys.join(", "));
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
        let _ = write!(sql, " WHERE ({filter})");
    }
    sql
}

/// Quick filters from a cell's context menu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterOp {
    Eq,
    Ne,
}

/// One condition on a column, e.g. `"name" = 'anna'`. A NULL value becomes IS (NOT) NULL.
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

/// Which rows a bulk update touches.
pub enum BulkScope<'a> {
    /// These rows, by primary key (`pk` names the columns, each row has one value per column).
    Keys {
        pk: &'a [String],
        rows: &'a [Vec<Option<String>>],
    },
    /// Every row passing this filter (empty: the whole table).
    Filter(&'a str),
}

/// What the "new value" of a bulk update may not contain: it must stay one expression inside
/// one statement.
pub fn check_expression(expr: &str) -> Result<(), String> {
    let e = expr.trim();
    if e.is_empty() {
        return Err("Type the new value: a SQL expression such as price * 1.1, upper(name) or NULL".into());
    }
    if e.contains(';') || e.contains("--") || e.contains("/*") {
        return Err("One expression only: no ';' and no comments".into());
    }
    Ok(())
}

/// `UPDATE t SET col = <expression> WHERE …`, to be looked at (and previewed) before it runs.
pub fn bulk_update(
    kind: Kind,
    schema: &str,
    table: &str,
    column: &str,
    expression: &str,
    scope: &BulkScope,
) -> String {
    let target = kind.qualified(schema, table);
    let mut sql = format!(
        "UPDATE {target}\nSET {} = {}",
        kind.quote_ident(column),
        expression.trim()
    );
    let condition = match scope {
        BulkScope::Filter(f) if f.trim().is_empty() => String::new(),
        BulkScope::Filter(f) => format!("({})", f.trim()),
        BulkScope::Keys { pk, rows } => match pk {
            [] => String::new(),
            [only] => {
                let values: Vec<String> = rows
                    .iter()
                    .filter_map(|r| r.first().cloned().flatten())
                    .map(|v| kind.quote_str(&v))
                    .collect();
                format!("{} IN ({})", kind.quote_ident(only), values.join(", "))
            }
            _ => rows
                .iter()
                .map(|r| {
                    let parts: Vec<String> = pk
                        .iter()
                        .zip(r)
                        .map(|(c, v)| filter_condition(kind, c, FilterOp::Eq, v.as_deref()))
                        .collect();
                    format!("({})", parts.join(" AND "))
                })
                .collect::<Vec<_>>()
                .join("\n   OR "),
        },
    };
    if !condition.is_empty() {
        let _ = write!(sql, "\nWHERE {condition}");
    }
    sql.push(';');
    sql
}

/// `term` anywhere in `col`, its `%` and `_` taken literally. The escape is `!`, not the usual
/// backslash, which MySQL reads differently under `NO_BACKSLASH_ESCAPES`.
fn contains(kind: Kind, col: &str, term: &str) -> String {
    let mut escaped = String::with_capacity(term.len());
    for c in term.chars() {
        if matches!(c, '!' | '%' | '_') {
            escaped.push('!');
        }
        escaped.push(c);
    }
    let pattern = kind.quote_str(&format!("%{escaped}%"));
    match kind {
        Kind::Postgres => format!("CAST({col} AS TEXT) ILIKE {pattern} ESCAPE '!'"),
        // MySQL compares case-insensitively with the default collations.
        Kind::MySql => format!("CAST({col} AS CHAR) LIKE {pattern} ESCAPE '!'"),
        // SQLite's LIKE ignores case (for ASCII letters).
        Kind::Sqlite => format!("CAST({col} AS TEXT) LIKE {pattern} ESCAPE '!'"),
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

/// The WHERE box is pasted into generated SELECT / COUNT queries, and Postgres (MySQL too, with
/// multi-statements) runs every `;`-separated statement in one call: `1=1; DELETE FROM t;
/// SELECT 1 AS id` would delete rows without any confirmation. So the filter must be a single
/// condition: no `;` outside string literals and quoted identifiers, and no comments.
///
/// Where a literal ends depends on the server: a backslash escapes the quote in Postgres `E''`
/// strings and in MySQL by default, but not under `standard_conforming_strings` or
/// `NO_BACKSLASH_ESCAPES`. So the filter is read both ways and must pass both (SQLite has no
/// backslash escapes). Comments and Postgres dollar quotes, which could hide a quote from this
/// reading, aren't allowed at all.
pub fn check_filter(kind: Kind, filter: &str) -> Result<(), String> {
    let chars: Vec<char> = filter.chars().collect();
    let readings: &[bool] = if kind == Kind::Sqlite {
        &[false]
    } else {
        &[false, true]
    };
    for &backslash_escapes in readings {
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if matches!(c, '\'' | '"' | '`') {
                i += 1;
                while i < chars.len() && chars[i] != c {
                    if backslash_escapes && c != '`' && chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                // Left open, it would close on a quote of the query built around it.
                if i >= chars.len() {
                    return Err(format!("The filter has an unclosed {c} quote."));
                }
            } else if c == ';' {
                return Err("The filter accepts a single condition: \";\" isn't allowed.".into());
            } else if (c == '-' && next == Some('-'))
                || (c == '/' && next == Some('*'))
                || (kind == Kind::MySql && c == '#')
            {
                return Err("Comments (--, /* */ or #) aren't allowed in the filter.".into());
            } else if kind == Kind::Postgres && c == '$' && dollar_quote_at(&chars, i) {
                return Err("Dollar-quoted strings ($$...$$) aren't allowed in the filter.".into());
            }
            i += 1;
        }
    }
    Ok(())
}

/// A Postgres dollar quote (`$$`, `$tag$`) opens at `i`: not inside a name (`a$b` is one) and
/// not a parameter (`$1`).
fn dollar_quote_at(chars: &[char], i: usize) -> bool {
    let name_char = |c: char| c.is_alphanumeric() || c == '_';
    if i > 0 && (name_char(chars[i - 1]) || chars[i - 1] == '$') {
        return false;
    }
    if chars.get(i + 1).is_some_and(char::is_ascii_digit) {
        return false;
    }
    let mut j = i + 1;
    while j < chars.len() && name_char(chars[j]) {
        j += 1;
    }
    chars.get(j) == Some(&'$')
}

/// `INSERT` reproducing one row, for "copy as INSERT".
pub fn insert_row<'a>(
    kind: Kind,
    table: &str,
    columns: &[String],
    row: impl IntoIterator<Item = Option<&'a str>>,
) -> String {
    let cols = columns
        .iter()
        .map(|c| kind.quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let vals = row
        .into_iter()
        .map(|v| v.map_or("NULL".to_string(), |v| kind.quote_str(v)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO {table} ({cols}) VALUES ({vals});")
}

/// Parses what a user typed into a boolean cell. `None` if it isn't a recognizable boolean.
pub fn parse_bool(input: &str) -> Option<bool> {
    match input.trim().to_lowercase().as_str() {
        "true" | "t" | "1" | "yes" | "y" | "on" => Some(true),
        "false" | "f" | "0" | "no" | "n" | "off" => Some(false),
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

/// Most common values kept in `values::ColumnStats::top`.
pub const TOP_VALUES: usize = 10;

/// The two queries behind "Column statistics": totals (count, non-null, distinct and, with
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
            TOP_VALUES
        ),
    )
}

/// Named parameters (`:id`, `:name`) in a script: char ranges of each occurrence (colon
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
        // One sign at most: `--5` would start a comment in the SQL.
        let digits = t.strip_prefix('-').unwrap_or(t);
        let number = !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
            && t.chars().filter(|c| *c == '.').count() <= 1
            && t.chars().any(|c| c.is_ascii_digit());
        if t.eq_ignore_ascii_case("null") {
            out.push_str("NULL");
        } else if number && t.starts_with('-') {
            // `x-:v` would read `x--5`, a comment.
            out.push_str(&format!("({t})"));
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
/// `public."Table"` comes whole). `None` on whitespace or punctuation.
pub fn word_at(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '.' | '"' | '`');
    // Asked every frame the pointer rests on the editor: walks out from the cursor instead of
    // copying the whole text into chars.
    let at = text.char_indices().nth(cursor).map_or(text.len(), |(i, _)| i);
    // The pointer sits between chars: take the one after it, or the one before at a word's end.
    if !text[at..].chars().next().is_some_and(is_word) && !text[..at].chars().next_back().is_some_and(is_word)
    {
        return None;
    }
    let before = text[..at].chars().rev().take_while(|c| is_word(*c)).count();
    let after = text[at..].chars().take_while(|c| is_word(*c)).count();
    // Trailing dots ("table.") aren't part of the name.
    let word_end = text[at..]
        .char_indices()
        .nth(after)
        .map_or(text.len(), |(i, _)| at + i);
    let start = at
        - text[..at]
            .chars()
            .rev()
            .take(before)
            .map(char::len_utf8)
            .sum::<usize>();
    let dots = text[start..word_end]
        .chars()
        .rev()
        .take_while(|c| *c == '.')
        .count();
    let (a, b) = (cursor - before, cursor + after - dots);
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

/// `a integer, OUT total numeric, numeric(10,2)` (Postgres) or `IN pid int, IN qty int` (MySQL)
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

/// The call of a function or procedure with `:parameters` for its inputs. OUT arguments are
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
        (false, Kind::MySql | Kind::Sqlite) => format!("SELECT {call} AS result;"),
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
                let _ = write!(
                    s,
                    "\nINSERT INTO {new} OVERRIDING SYSTEM VALUE SELECT * FROM {t};"
                );
            }
            s
        }
        (TableAction::Duplicate(data), Kind::MySql) => {
            let mut s = format!("CREATE TABLE {new} LIKE {t};");
            if data {
                let _ = write!(s, "\nINSERT INTO {new} SELECT * FROM {t};");
            }
            s
        }
        (TableAction::Rename, Kind::Sqlite) => {
            format!("ALTER TABLE {t} RENAME TO {};", kind.quote_ident(name.trim()))
        }
        // No CREATE TABLE … LIKE in SQLite: the columns (and rows) come, the rest doesn't.
        (TableAction::Duplicate(data), Kind::Sqlite) => format!(
            "-- SQLite: the copy gets the columns{}, but not the primary key, indexes or constraints.\n\
             CREATE TABLE {new} AS SELECT * FROM {t}{};",
            if data { " and the data" } else { "" },
            if data { "" } else { " WHERE 0" }
        ),
        (TableAction::Truncate, Kind::Sqlite) => format!("DELETE FROM {t};"),
        (TableAction::Truncate, _) => format!("TRUNCATE TABLE {t};"),
        (TableAction::Drop, _) => format!("DROP TABLE {t};"),
    }
}

/// What "Generate SQL" writes for a table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Template {
    Select,
    Insert,
    Update,
    Delete,
}

/// A statement for a table, with `:parameters` where values go (the editor asks for them when
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
/// unquoted. What it selects is checked once the table's columns are known (see
/// `result_matches_table`).
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

/// What one item of a select list gives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SelectItem {
    /// `*` or `t.*`: every column of the table.
    All,
    /// `name`, `t."Name"`, `name AS name`: one column under its own name.
    Column,
    /// Anything else: an expression, or a column under another name.
    Other,
}

/// The items of a SELECT's list, in order. `None` when it can't be read for sure (comments and
/// backslashes, which servers read differently).
pub fn select_items(statement: &str) -> Option<Vec<SelectItem>> {
    let sql = statement.trim().trim_end_matches(';').trim_end();
    let words = top_level_words(sql);
    if words.first()?.0 != "SELECT" {
        return None;
    }
    let from = words.iter().position(|(w, _, _)| w == "FROM")?;
    let list: String = sql
        .chars()
        .skip(words[0].2)
        .take(words[from].1 - words[0].2)
        .collect();
    if list.contains(['\\', '#']) || list.contains("--") || list.contains("/*") {
        return None;
    }
    // Commas outside quotes and parentheses.
    let mut items = vec![String::new()];
    let (mut depth, mut quote) = (0i32, None);
    for c in list.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ',') if depth == 0 => {
                items.push(String::new());
                continue;
            }
            _ => {}
        }
        items.last_mut().unwrap().push(c);
    }
    Some(items.iter().map(|i| select_item(i)).collect())
}

fn select_item(item: &str) -> SelectItem {
    if item.contains(['\'', '(', ')']) {
        return SelectItem::Other;
    }
    let quoted = |p: &str| p.starts_with(['"', '`']);
    let is_name = |p: &str| match p.chars().next() {
        Some(q @ ('"' | '`')) => p.len() >= 2 && p.ends_with(q),
        Some(first) => {
            (first.is_alphabetic() || first == '_')
                && p.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        }
        None => false,
    };
    let words: Vec<String> = split_outside_quotes(item, char::is_whitespace)
        .into_iter()
        .filter(|w| !w.is_empty())
        .collect();
    let (name, alias) = match &words[..] {
        [n] => (n, None),
        [n, a] => (n, Some(a)),
        [n, as_, a] if as_.eq_ignore_ascii_case("AS") => (n, Some(a)),
        _ => return SelectItem::Other,
    };
    let parts = split_dotted(name);
    let (last, path) = parts.split_last().expect("split_dotted gives one part at least");
    if parts.len() > 3 || !path.iter().all(|p| is_name(p)) {
        return SelectItem::Other;
    }
    if last == "*" {
        return if alias.is_none() {
            SelectItem::All
        } else {
            SelectItem::Other
        };
    }
    // The same name: spelled alike, or both unquoted (folded alike by the server).
    let same =
        |a: &String| a == last || (!quoted(a) && !quoted(last) && is_name(a) && a.eq_ignore_ascii_case(last));
    if is_name(last) && alias.is_none_or(same) {
        SelectItem::Column
    } else {
        SelectItem::Other
    }
}

/// Whether an editor's result can be edited as rows of `table` (its column names): every result
/// column named like one of the table's must be that column. Edits find their row by the key
/// columns' values and write a cell under its column's name, so `name AS id`, `id + 1 AS id` or
/// `count(*) AS id` would aim them at another row or column; `upper(name) AS shout` is fine
/// (that column just can't be edited). False too when the items don't line up with `result`.
pub fn result_matches_table(items: &[SelectItem], table: &[String], result: &[String]) -> bool {
    let mut i = 0;
    for item in items {
        match item {
            SelectItem::All => {
                let Some(all) = result.get(i..i + table.len()) else {
                    return false;
                };
                if !all.iter().all(|c| table.contains(c)) {
                    return false;
                }
                i += table.len();
            }
            SelectItem::Column => i += 1,
            SelectItem::Other => match result.get(i) {
                Some(name) if !table.contains(name) => i += 1,
                _ => return false,
            },
        }
    }
    i == result.len()
}

/// `s` cut at each char `split` matches outside `"` and `` ` `` quotes.
fn split_outside_quotes(s: &str, split: impl Fn(char) -> bool) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut quote = None;
    for c in s.chars() {
        match (quote, c) {
            (None, '"' | '`') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, c) if split(c) => {
                parts.push(String::new());
                continue;
            }
            _ => {}
        }
        parts.last_mut().unwrap().push(c);
    }
    parts
}

/// `a.b`, `"a.x"."b"`: splits on dots outside quotes.
fn split_dotted(name: &str) -> Vec<String> {
    split_outside_quotes(name, |c| c == '.')
}

/// Reads for "preview" of an UPDATE or DELETE, without running it: how many rows it would
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
            // From the end of UPDATE (or its modifier): a quoted name isn't one of `words`.
            let table = text(words.get(t0 - 1)?.2, words[set].1);
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
            let table = text(words.get(t0 - 1)?.2, end);
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
    /// Human descriptions of destructive statements, e.g. "DELETE without WHERE".
    pub dangers: Vec<String>,
}

pub fn script_risk(script: &str) -> ScriptRisk {
    const READS: &[&str] = &["SELECT", "SHOW", "EXPLAIN", "DESCRIBE", "DESC", "VALUES", "TABLE"];
    const MODIFIES: &[&str] = &["INSERT", "UPDATE", "DELETE", "MERGE"];
    // Functions a SELECT can call that change something: sequences, locks, other sessions, files,
    // notifications. (A user's own function can do anything; only the server's grants tell.)
    const SIDE_EFFECTS: &[&str] = &[
        "NEXTVAL",
        "SETVAL",
        "PG_ADVISORY_LOCK",
        "PG_ADVISORY_XACT_LOCK",
        "PG_TRY_ADVISORY_LOCK",
        "PG_TRY_ADVISORY_XACT_LOCK",
        "PG_ADVISORY_UNLOCK",
        "PG_ADVISORY_UNLOCK_ALL",
        "PG_TERMINATE_BACKEND",
        "PG_CANCEL_BACKEND",
        "PG_RELOAD_CONF",
        "PG_ROTATE_LOGFILE",
        "PG_SWITCH_WAL",
        "PG_CREATE_RESTORE_POINT",
        "PG_NOTIFY",
        "PG_LOGICAL_EMIT_MESSAGE",
        "LO_IMPORT",
        "LO_EXPORT",
        "LO_CREATE",
        "LO_UNLINK",
        "DBLINK_EXEC",
        "GET_LOCK",
        "RELEASE_LOCK",
        "RELEASE_ALL_LOCKS",
    ];
    let mut risk = ScriptRisk::default();
    for words in statement_words(script) {
        let first = words[0].as_str();
        let has = |w: &str| words.iter().any(|x| x == w);
        let follows = |a: &str, b: &[&str]| words.windows(2).any(|p| p[0] == a && b.contains(&p[1].as_str()));
        // WITH ... is a read unless one of its parts modifies data (Postgres allows that).
        let read = READS.contains(&first) || (first == "WITH" && !MODIFIES.iter().any(|m| has(m)));
        // Reads that still change something: SELECT INTO (a new table, or variables), locking
        // reads (their locks need the transaction), and calls with side effects.
        let changes = has("INTO")
            || follows("FOR", &["UPDATE", "SHARE", "NO", "KEY"])
            || follows("LOCK", &["IN"])
            || words.iter().any(|w| SIDE_EFFECTS.contains(&w.as_str()));
        if !read || changes {
            risk.writes = true;
        }
        match first {
            "TRUNCATE" => risk.dangers.push("TRUNCATE".into()),
            "DROP" | "ALTER" => {
                let object = words.get(1).map(String::as_str).unwrap_or("");
                risk.dangers.push(format!("{first} {object}").trim().to_string());
            }
            "DELETE" | "UPDATE" if !has("WHERE") => risk.dangers.push(format!("{first} without WHERE")),
            _ => {}
        }
    }
    // Reads that still change things: Postgres' set_config can turn the session's read-only
    // mode off, MySQL's INTO OUTFILE/DUMPFILE writes files on the server. Looked for in the
    // raw text so quoting the name doesn't hide it. (A guard against accidents: a server user
    // with only SELECT grants is what really keeps a connection read-only.)
    let lower = script.to_ascii_lowercase();
    if ["set_config", "outfile", "dumpfile"]
        .iter()
        .any(|w| lower.contains(w))
    {
        risk.writes = true;
    }
    risk
}

/// The char range of `script`'s statement when it is a single read a cursor can fetch a few
/// rows of (SELECT, VALUES, TABLE, a WITH that only reads). Not a locking read (a cursor locks
/// only the rows fetched) nor SELECT INTO (it creates a table; a cursor refuses it).
pub fn single_read(script: &str) -> Option<(usize, usize)> {
    let chars: Vec<char> = script.chars().collect();
    // A comment after the last `;` is a range of its own, with no words.
    let statements: Vec<_> = statement_ranges(script, false)
        .into_iter()
        .filter_map(|(a, b)| {
            let words = statement_words(&chars[a..b].iter().collect::<String>())
                .into_iter()
                .next()
                .unwrap_or_default();
            (!words.is_empty()).then_some(((a, b), words))
        })
        .collect();
    let [(range, words)] = &statements[..] else {
        return None;
    };
    let range = *range;
    let reads = ["SELECT", "VALUES", "TABLE", "WITH"].contains(&words.first()?.as_str());
    let locks_or_creates = words
        .iter()
        .any(|w| matches!(w.as_str(), "INTO" | "UPDATE" | "SHARE"));
    (reads && !locks_or_creates && !script_risk(script).writes).then_some(range)
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

/// Values "Filter values" lists at most (the most common ones).
pub const DISTINCT_LIMIT: usize = 1000;

/// The column as "Filter values" compares it: Postgres `json`/`xml` have no equality, so as text.
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
        // Parenthesized so the filter stays one condition (see `check_filter`).
        format!(" WHERE ({cond})")
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

/// `filter` without `cond`, when `cond` was added with `and`: anywhere in the chain `and`
/// makes (`((f) AND c1) AND c2`), first, last or in between. `None` when it isn't there any
/// more (the user edited the filter).
pub fn remove_condition(filter: &str, cond: &str) -> Option<String> {
    let mut parts = and_parts(filter);
    let at = parts.iter().position(|p| p == cond.trim())?;
    parts.remove(at);
    Some(parts.iter().fold(String::new(), |f, p| and(&f, p)))
}

/// The conditions `and` joined, first to last: `((a) AND b) AND c` → `a`, `b`, `c`.
fn and_parts(filter: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = filter.trim().to_string();
    while let Some((left, right)) = split_and(&rest) {
        parts.push(right);
        rest = left;
    }
    parts.push(rest);
    parts.reverse();
    parts
}

/// `(left) AND right` → `(left, right)`. Not when `right` has an OR of its own: `(a) AND b OR c`
/// means `((a) AND b) OR c`.
fn split_and(f: &str) -> Option<(String, String)> {
    let chars: Vec<char> = f.chars().collect();
    if chars.first() != Some(&'(') {
        return None;
    }
    let (mut depth, mut quote, mut close) = (0, None, None);
    for (i, &c) in chars.iter().enumerate() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let after: String = chars[close + 1..].iter().collect();
    let right = after.strip_prefix(" AND ")?.trim();
    if right.is_empty() || top_level_words(right).iter().any(|(w, _, _)| w == "OR") {
        return None;
    }
    let left: String = chars[1..close].iter().collect();
    Some((left.trim().to_string(), right.to_string()))
}

/// Referenced tables up to this many rows are loaded whole for the foreign-key picker and
/// filtered as you type; bigger ones are searched in the database.
pub const FK_LOCAL_ROWS: i64 = 10_000;
/// Where a foreign key column points.
#[derive(Clone, Debug, PartialEq)]
pub struct FkRef {
    pub schema: String,
    pub table: String,
    pub column: String,
}

/// Rows a database search of the picker brings.
pub const FK_SEARCH_ROWS: usize = 50;

/// The rows the foreign-key picker lists: `(key, label)` of the referenced table. `search`
/// empty: the start of the table (all of it when `local`). A number typed matches the key
/// exactly (instant, by the primary key index); text is looked for inside the label.
pub fn fk_options(
    kind: Kind,
    fk: &FkRef,
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
        "name",
        "full_name",
        "display_name",
        "title",
        "description",
        "email",
        "login",
        "username",
        "sku",
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
    /// What the editor runs on every text, every frame: no panic, and ranges inside the text.
    #[test]
    fn scanners_take_any_text() {
        use crate::testgen::each_text;
        each_text(1, 3000, |s, g| {
            let n = s.chars().count();
            for blank in [false, true] {
                let ranges = statement_ranges(s, blank);
                let mut end = 0;
                for &(a, b) in &ranges {
                    assert!(end <= a && a < b && b <= n, "{ranges:?} of {n} chars");
                    end = b;
                }
                for c in [0, n / 2, n, g.below(n + 1)] {
                    if let Some(r) = statement_in(&ranges, c) {
                        assert!(ranges.contains(&r));
                    }
                }
            }
            for c in [0, n, g.below(n + 1)] {
                // The word may end before the cursor: trailing dots aren't part of it.
                if let Some((a, b)) = word_at(s, c) {
                    assert!(a <= c && a < b && b <= n, "word {a}..{b} at {c} of {n}");
                }
            }
            for (a, b, name) in param_spans(s) {
                assert!(a < b && b <= n && !name.is_empty());
            }
            let _ = bind_params(Kind::Postgres, s, |p| format!("'{p}'"));
            if let Some((a, b)) = single_read(s) {
                assert!(statement_ranges(s, false).contains(&(a, b)));
                assert!(!script_risk(s).writes, "a single read doesn't write");
            }
            let _ = (
                tx_control(s),
                script_risk(s),
                mysql_implicit_commit(s),
                statement_words_first(s),
            );
            let _ = (
                editable_select(s),
                preview_queries(s),
                parse_args(s),
                parse_bool(s),
            );
            let _ = (
                check_filter(Kind::Postgres, s),
                check_filter(Kind::MySql, s),
                check_expression(s),
                remove_condition(s, "x = 1"),
            );
            let _ = and(s, "x = 1");
        });
    }

    /// The formatter is a dependency: whatever it makes of broken SQL, the editor survives it.
    #[test]
    fn format_takes_any_text() {
        crate::testgen::each_text(2, 1500, |s, _| {
            for kind in [Kind::Postgres, Kind::MySql, Kind::Sqlite] {
                let _ = format_sql(kind, s);
            }
        });
    }

    #[test]
    fn single_reads() {
        let range = |s: &str| single_read(s).map(|(a, b)| s.chars().skip(a).take(b - a).collect::<String>());
        assert_eq!(
            range("  SELECT * FROM t;  -- done").as_deref(),
            Some("SELECT * FROM t")
        );
        assert_eq!(
            range("with x as (select 1) select * from x").as_deref(),
            Some("with x as (select 1) select * from x")
        );
        assert!(range("VALUES (1), (2)").is_some());
        assert!(range("TABLE t").is_some());
        assert!(
            range("SELECT 'update' FROM t").is_some(),
            "words in quotes don't count"
        );
        for no in [
            "SELECT 1; SELECT 2",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM t FOR SHARE",
            "SELECT * INTO copy FROM t",
            "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
            "SELECT set_config('default_transaction_read_only', 'off', false)",
            "SHOW search_path",
            "UPDATE t SET a = 1",
            "",
        ] {
            assert_eq!(range(no), None, "{no}");
        }
    }

    #[test]
    fn previews_read_quoted_table_names() {
        let q = preview_queries("UPDATE \"public\".\"My Table\"\nSET \"a\" = a + 1\nWHERE \"id\" IN ('1');")
            .unwrap();
        assert_eq!(
            q.count,
            "SELECT count(*) FROM \"public\".\"My Table\" WHERE \"id\" IN ('1')"
        );
        let q = preview_queries("DELETE FROM `db`.`t` WHERE x = 1").unwrap();
        assert_eq!(q.count, "SELECT count(*) FROM `db`.`t` WHERE x = 1");
        // Unquoted names and MySQL's modifiers keep working.
        let q = preview_queries("UPDATE LOW_PRIORITY t SET a = 1").unwrap();
        assert_eq!(q.count, "SELECT count(*) FROM t");
        let q = preview_queries("UPDATE ONLY s.t x SET a = 1 WHERE b").unwrap();
        assert_eq!(q.count, "SELECT count(*) FROM s.t x WHERE b");
    }

    #[test]
    fn bulk_updates_name_their_rows_by_key_or_filter() {
        let pk = vec!["id".to_string()];
        let rows = vec![vec![Some("1".into())], vec![Some("7".into())]];
        let sql = bulk_update(
            Kind::Postgres,
            "public",
            "products",
            "price",
            " price * 1.1 ",
            &BulkScope::Keys { pk: &pk, rows: &rows },
        );
        assert_eq!(
            sql,
            "UPDATE \"public\".\"products\"\nSET \"price\" = price * 1.1\nWHERE \"id\" IN ('1', '7');"
        );
        // A composite key becomes an OR of the key pairs.
        let pk2 = vec!["a".to_string(), "b".to_string()];
        let rows2 = vec![vec![Some("1".into()), Some("x".into())]];
        let sql = bulk_update(
            Kind::MySql,
            "db",
            "t",
            "v",
            "NULL",
            &BulkScope::Keys {
                pk: &pk2,
                rows: &rows2,
            },
        );
        assert!(sql.ends_with("WHERE (`a` = '1' AND `b` = 'x');"), "{sql}");
        // A filter stays in one piece; no filter means the whole table.
        let sql = bulk_update(
            Kind::Postgres,
            "s",
            "t",
            "c",
            "0",
            &BulkScope::Filter("a > 1 OR b"),
        );
        assert!(sql.ends_with("WHERE (a > 1 OR b);"), "{sql}");
        let sql = bulk_update(Kind::Postgres, "s", "t", "c", "0", &BulkScope::Filter(""));
        assert!(!sql.contains("WHERE"), "{sql}");
    }

    #[test]
    fn the_new_value_must_be_one_expression() {
        assert!(check_expression("price * 1.1").is_ok());
        assert!(check_expression("coalesce(x, 0) + 1").is_ok());
        assert!(check_expression("   ").is_err());
        assert!(check_expression("1; DROP TABLE t").is_err());
        assert!(check_expression("1 -- comment").is_err());
        assert!(check_expression("1 /* c */").is_err());
    }

    #[test]
    fn several_sort_columns_keep_their_order_and_the_key_follows() {
        let pk = vec!["id".to_string()];
        let q = PageQuery {
            schema: "public",
            table: "t",
            filter: "",
            order: &[
                ("status".into(), true),
                ("created_at".into(), false),
                ("id".into(), true),
            ],
            pk: &pk,
            page: 0,
        };
        assert_eq!(
            select_page(Kind::Postgres, &q),
            "SELECT * FROM \"public\".\"t\" ORDER BY \"status\" ASC, \"created_at\" DESC, \"id\" ASC LIMIT 200 OFFSET 0"
        );
        // The key is added only when no sort column is it already.
        let q = PageQuery {
            order: &[("status".into(), false)],
            ..q
        };
        assert!(select_page(Kind::Postgres, &q).contains("ORDER BY \"status\" DESC, \"id\" ASC"));
    }

    use super::*;

    #[test]
    fn page_query() {
        let q = PageQuery {
            schema: "public",
            table: "t",
            filter: " id > 1 ",
            order: &[("na\"me".into(), false)],
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
            order: &[],
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
            order: &[],
            pk: &pk,
            page: 0,
        };
        assert_eq!(
            select_page(Kind::Postgres, &q),
            "SELECT * FROM \"s\".\"t\" ORDER BY \"id\" ASC LIMIT 200 OFFSET 0"
        );
        let q = PageQuery {
            order: &[("name".into(), false)],
            ..q
        };
        assert!(select_page(Kind::MySql, &q).contains("ORDER BY `name` DESC, `id` ASC"));
        let q = PageQuery {
            order: &[("id".into(), false)],
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
        let cols = vec!["id".to_string(), "name".to_string()];
        assert_eq!(
            insert_row(Kind::Postgres, "\"public\".\"t\"", &cols, [Some("1"), None]),
            "INSERT INTO \"public\".\"t\" (\"id\", \"name\") VALUES ('1', NULL);"
        );
    }

    #[test]
    fn booleans_per_engine() {
        assert_eq!(parse_bool(" TRUE "), Some(true));
        assert_eq!(parse_bool("f"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
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
        assert_eq!(r.dangers, ["DELETE without WHERE", "DROP TABLE"]);
        // Words inside strings and comments don't count.
        let r = script_risk("UPDATE t SET note = 'no where here' /* where */");
        assert_eq!(r.dangers, ["UPDATE without WHERE"]);
        assert!(script_risk("insert into t values (1)").writes);
        assert!(script_risk("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d").writes);
        // Reads that undo read-only mode or write server files.
        assert!(script_risk("SELECT set_config('default_transaction_read_only', 'off', false)").writes);
        assert!(script_risk("select \"SET_CONFIG\"('x', 'y', false)").writes);
        assert!(script_risk("SELECT * FROM t INTO OUTFILE '/tmp/x'").writes);
        assert!(script_risk("SELECT * INTO copy FROM t").writes);
        assert!(script_risk("select * from t where id = 1 for update").writes);
        assert!(script_risk("SELECT * FROM t FOR NO KEY UPDATE").writes);
        assert!(script_risk("SELECT * FROM t LOCK IN SHARE MODE").writes);
        assert!(script_risk("select nextval('orders_id_seq')").writes);
        assert!(script_risk("SELECT pg_terminate_backend(42)").writes);
        assert!(script_risk("SELECT GET_LOCK('x', 1)").writes);
        assert!(
            !script_risk("SELECT nextval_note, \"for\" FROM t").writes,
            "names that only look alike"
        );
        assert_eq!(script_risk("select ';' ; truncate t").dangers, ["TRUNCATE"]);
    }

    #[test]
    fn filter_must_be_one_condition() {
        const KINDS: [Kind; 3] = [Kind::Postgres, Kind::MySql, Kind::Sqlite];
        for k in KINDS {
            assert!(check_filter(k, "id > 10 AND name LIKE 'a%'").is_ok());
            assert!(check_filter(k, "name = 'a;b' OR \"we;ird\" = 1").is_ok());
            assert!(check_filter(k, "1=1; DELETE FROM t; SELECT 1 AS id").is_err());
            assert!(check_filter(k, "1=1 -- ").is_err());
            assert!(check_filter(k, "x = 'it''s -- fine'").is_ok());
            assert!(check_filter(k, "path = 'C:\\dir' AND x > 1").is_ok());
            // A quote inside a comment hides the `;` from a reading that skips no comments.
            let comment = "1=1 /*'*/); DELETE FROM t; SELECT 1 WHERE (1=1 /*'*/";
            assert!(check_filter(k, comment).is_err(), "{k:?}");
            // An open quote would close on one of the query's own.
            for open in ["x = 'abc", "x = \"abc", "x = `abc", "x = 'it''s"] {
                assert!(check_filter(k, open).is_err(), "{k:?} {open}");
            }
        }
        for k in [Kind::Postgres, Kind::MySql] {
            // `\'` ends the literal for one server setting and not for the other: the `;` is
            // outside for one of them, so it's refused.
            let escaped = "x = E'\\'') ; DELETE FROM t; SELECT * FROM t WHERE (''=''";
            assert!(check_filter(k, escaped).is_err(), "{k:?}");
            assert!(check_filter(k, "x = 'C:\\' OR y = ';'").is_err(), "{k:?}");
        }
        // SQLite reads `\` as any other char.
        assert!(check_filter(Kind::Sqlite, "x = 'C:\\' OR y = ';'").is_ok());
        // Dollar quotes are Postgres strings; `$1` and `a$b` are not.
        let dollar = "x = $$'$$) ; DELETE FROM t; SELECT * FROM t WHERE ($$'$$";
        assert!(check_filter(Kind::Postgres, dollar).is_err());
        assert!(check_filter(Kind::Postgres, "x = $tag$a$tag$").is_err());
        assert!(check_filter(Kind::Postgres, "a$b = 1 AND c = $1").is_ok());
        // `#` starts a MySQL comment, but is an operator in Postgres.
        assert!(check_filter(Kind::MySql, "1=1 # '\n; DELETE FROM t").is_err());
        assert!(check_filter(Kind::Postgres, "data #>> '{a}' = 'x'").is_ok());
    }

    /// A filter that passes the check stays one statement in every query built around it, as
    /// the editor splits scripts. Postgres is the one that runs every statement of a query
    /// (and the splitter reads its `$$` quotes, which other engines don't have).
    #[test]
    fn checked_filters_stay_one_statement() {
        let k = Kind::Postgres;
        // The search and the key's name are quoted on their own; they must stay so next to
        // any filter that passes.
        let pk = ["a\";DELETE FROM t;--".to_string()];
        crate::testgen::each_text(11, 3000, |f, _| {
            if check_filter(k, f).is_err() {
                return;
            }
            let q = PageQuery {
                schema: "s",
                table: "t",
                filter: f,
                order: &[],
                pk: &pk,
                page: 0,
            };
            for sql in [
                select_page(k, &q),
                count(k, "s", "t", f),
                distinct_values(k, "s", "t", "\"c\"", f, ""),
                distinct_values(k, "s", "t", "\"c\"", f, "';DELETE FROM t;--"),
            ] {
                assert!(statement_ranges(&sql, false).len() <= 1, "{sql}");
            }
        });
    }

    #[test]
    fn insert_only_filled_columns() {
        let v = vec![
            ("name".to_string(), Some("anna".to_string())),
            ("active".to_string(), None),
        ];
        let (sql, vals) = insert_values(Kind::MySql, "s", "t", &v).with_placeholders();
        assert_eq!(sql, "INSERT INTO `s`.`t` (`name`, `active`) VALUES (?, NULL)");
        assert_eq!(vals, ["anna"]);
        assert_eq!(
            insert_values(Kind::Postgres, "s", "t", &[]).render(Kind::Postgres),
            "INSERT INTO \"s\".\"t\" DEFAULT VALUES"
        );
    }

    #[test]
    fn formatting() {
        let raw = "select id, name from customers c join orders p on p.customer_id = c.id where name = 'select from' and active order by id";
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
        let (sum, top) = column_stats(Kind::Postgres, "public", "t", "unit price", "a > 1", true);
        assert_eq!(
            sum,
            "SELECT count(*), count(\"unit price\"), count(DISTINCT \"unit price\"::text), min(\"unit price\"), max(\"unit price\") FROM \"public\".\"t\" WHERE (a > 1)"
        );
        assert_eq!(
            top,
            "SELECT \"unit price\"::text, count(*) FROM \"public\".\"t\" WHERE (a > 1) GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT 10"
        );
        let (sum, _) = column_stats(Kind::MySql, "db", "t", "c", "", false);
        assert_eq!(
            sum,
            "SELECT count(*), count(`c`), count(DISTINCT `c`) FROM `db`.`t`"
        );
    }

    #[test]
    fn named_parameters() {
        let q = "SELECT x::text, ':no' AS s, a[1:2], @v := 1 -- :also_not\nFROM t WHERE id = :id AND (name = :name OR :id IS NULL)";
        assert_eq!(param_names(q), ["id", "name"]);
        let bound = bind_params(Kind::Postgres, q, |n| {
            if n == "id" { "42".into() } else { "O'Neil".into() }
        });
        assert!(
            bound.ends_with("WHERE id = 42 AND (name = 'O''Neil' OR 42 IS NULL)"),
            "{bound}"
        );
        assert!(bound.starts_with("SELECT x::text, ':no'"));
        assert_eq!(bind_params(Kind::MySql, "a = :v", |_| "null".into()), "a = NULL");
        assert_eq!(
            bind_params(Kind::MySql, "a = :v", |_| "-1.5".into()),
            "a = (-1.5)"
        );
        // Never a comment: `--` isn't a number, and a negative one keeps away from a minus.
        assert_eq!(bind_params(Kind::MySql, "a = :v", |_| "--5".into()), "a = '--5'");
        assert_eq!(bind_params(Kind::Postgres, "x-:v", |_| "-5".into()), "x-(-5)");
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
            a(Kind::Postgres, TableAction::Rename, "new"),
            "ALTER TABLE \"s\".\"t\" RENAME TO \"new\";"
        );
        assert_eq!(
            a(Kind::MySql, TableAction::Rename, "new"),
            "RENAME TABLE `s`.`t` TO `s`.`new`;"
        );
        assert_eq!(
            a(Kind::Postgres, TableAction::Duplicate(true), "copy"),
            "CREATE TABLE \"s\".\"copy\" (LIKE \"s\".\"t\" INCLUDING ALL);\nINSERT INTO \"s\".\"copy\" OVERRIDING SYSTEM VALUE SELECT * FROM \"s\".\"t\";"
        );
        assert_eq!(
            a(Kind::MySql, TableAction::Duplicate(false), "copy"),
            "CREATE TABLE `s`.`copy` LIKE `s`.`t`;"
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
        let my = parse_args("IN pid int, OUT new int");
        assert_eq!(
            call_template(Kind::MySql, "shop", "restock", &my, true),
            "CALL `shop`.`restock`(:pid, @new);\nSELECT @new;"
        );
        assert_eq!(
            call_template(Kind::MySql, "shop", "f", &[], false),
            "SELECT `shop`.`f`() AS result;"
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
            ("name".to_string(), false),
            ("birth date".to_string(), false),
        ];
        let t = |w| template(Kind::Postgres, "public", "customers", &cols, w);
        assert_eq!(
            t(Template::Update),
            "UPDATE \"public\".\"customers\"\nSET \"name\" = :name,\n    \"birth date\" = :birth_date\nWHERE \"id\" = :id;"
        );
        assert_eq!(
            t(Template::Delete),
            "DELETE FROM \"public\".\"customers\"\nWHERE \"id\" = :id;"
        );
        assert_eq!(
            t(Template::Insert),
            "INSERT INTO \"public\".\"customers\" (\"id\", \"name\", \"birth date\")\nVALUES (:id, :name, :birth_date);"
        );
        assert!(t(Template::Select).starts_with("SELECT \"id\",\n       \"name\""));
        // The parameters are the ones the editor will ask for.
        assert_eq!(param_names(&t(Template::Update)), ["name", "birth_date", "id"]);
        // No key: every column identifies the row.
        let nokey = [("a".to_string(), false), ("b".to_string(), false)];
        assert_eq!(
            template(Kind::MySql, "db", "t", &nokey, Template::Delete),
            "DELETE FROM `db`.`t`\nWHERE `a` = :a\n  AND `b` = :b;"
        );
    }

    #[test]
    fn words_under_the_pointer() {
        let text = "SELECT * FROM public.\"Customers\" c WHERE c.id = 1";
        let w =
            |i: usize| word_at(text, i).map(|(a, b)| text.chars().skip(a).take(b - a).collect::<String>());
        assert_eq!(w(16).as_deref(), Some("public.\"Customers\""));
        assert_eq!(
            w(31).as_deref(),
            Some("public.\"Customers\""),
            "right after the word"
        );
        assert_eq!(w(7), None, "on punctuation");
        assert_eq!(w(0).as_deref(), Some("SELECT"));
        assert_eq!(w(text.chars().count()).as_deref(), Some("1"), "at the very end");
        // Past non-ASCII text, char positions still line up.
        let text = "-- café ação\nSELECT * FROM pedidos. WHERE";
        let w =
            |i: usize| word_at(text, i).map(|(a, b)| text.chars().skip(a).take(b - a).collect::<String>());
        assert_eq!(w(6).as_deref(), Some("café"));
        assert_eq!(w(29).as_deref(), Some("pedidos"), "without the trailing dot");
        assert_eq!(w(35).as_deref(), Some("pedidos"), "right after the dot");
        assert_eq!(w(36).as_deref(), Some("WHERE"));
        assert_eq!(word_at("a . b", 2), None, "a dot alone is no name");
    }

    #[test]
    fn finds_the_table_of_editable_selects() {
        let t = |s: &str| editable_select(s);
        assert_eq!(
            t("SELECT * FROM customers WHERE state = 'NY' ORDER BY id LIMIT 10;"),
            Some((None, "customers".into()))
        );
        assert_eq!(
            t("select id, name from public.customers c where c.active"),
            Some((Some("public".into()), "customers".into()))
        );
        assert_eq!(
            t("SELECT * FROM \"My Schema\".\"Table.X\" AS t"),
            Some((Some("My Schema".into()), "Table.X".into()))
        );
        assert_eq!(
            t("SELECT * FROM `shop`.`products`"),
            Some((Some("shop".into()), "products".into()))
        );
        // A subquery in WHERE is fine; it isn't in FROM.
        assert!(
            t("SELECT * FROM orders WHERE customer_id IN (SELECT id FROM customers JOIN x ON true)")
                .is_some()
        );
        for no in [
            "SELECT * FROM a JOIN b ON a.id = b.id",
            "SELECT * FROM a, b",
            "SELECT state, count(*) FROM customers GROUP BY state",
            "SELECT DISTINCT state FROM customers",
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
        let p = preview_queries("UPDATE customers c SET balance = (SELECT 1 WHERE x) WHERE c.state = 'NY' AND (a OR b) ORDER BY id LIMIT 10;").unwrap();
        assert_eq!(
            p.count,
            "SELECT count(*) FROM customers c WHERE c.state = 'NY' AND (a OR b)"
        );
        assert_eq!(
            p.sample,
            "SELECT * FROM customers c WHERE c.state = 'NY' AND (a OR b) LIMIT 50"
        );
        let p = preview_queries("delete from public.orders where status = 'where' returning id").unwrap();
        assert_eq!(
            p.count,
            "SELECT count(*) FROM public.orders where status = 'where'"
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
            filter_condition(k, "name", FilterOp::Eq, Some("O'Neil")),
            "\"name\" = 'O''Neil'"
        );
        assert_eq!(filter_condition(k, "x", FilterOp::Eq, None), "\"x\" IS NULL");
        assert_eq!(
            filter_condition(k, "x", FilterOp::Ne, Some("1")),
            "(\"x\" <> '1' OR \"x\" IS NULL)"
        );
        assert_eq!(
            search_condition(Kind::MySql, &["x".into()], "an"),
            "(CAST(`x` AS CHAR) LIKE '%an%' ESCAPE '!')"
        );
        // Generated conditions always pass the WHERE-box check, whatever the value holds.
        for v in [
            "a;b",
            "x -- y",
            "it's",
            "a\\'; DROP TABLE t; --",
            "\\",
            "C:\\';",
            "$$;$$",
            "# ;",
            "/* ; */",
        ] {
            for kind in [Kind::Postgres, Kind::MySql, Kind::Sqlite] {
                let some = |x: &str| Some(x.to_string());
                for c in [
                    filter_condition(kind, "c", FilterOp::Eq, Some(v)),
                    filter_condition(kind, "c", FilterOp::Ne, Some(v)),
                    values_condition(kind, "\"c\"", &[some(v), some("b")], &[some("a;")], false).unwrap(),
                ] {
                    assert!(check_filter(kind, &c).is_ok(), "{kind:?} {c}");
                }
            }
        }
        assert_eq!(and("", "a = 1"), "a = 1");
        assert_eq!(and("b OR c", "a = 1"), "(b OR c) AND a = 1");
        assert_eq!(and("b", ""), "b");
        let s = search_condition(k, &["id".into(), "name".into()], " anna ");
        assert_eq!(
            s,
            "(CAST(\"id\" AS TEXT) ILIKE '%anna%' ESCAPE '!' OR CAST(\"name\" AS TEXT) ILIKE '%anna%' ESCAPE '!')"
        );
        assert_eq!(search_condition(k, &["id".into()], "  "), "");
    }

    #[test]
    fn quoting() {
        assert_eq!(Kind::Postgres.quote_str("o'x"), "'o''x'");
        // Backslashes switch to E'' so the literal means the same under any server setting.
        assert_eq!(Kind::Postgres.quote_str("o'x\\"), "E'o''x\\\\'");
        assert_eq!(Kind::MySql.quote_str("o'x"), "'o''x'");
        // `o'x\`: the same bytes under any sql_mode.
        assert_eq!(Kind::MySql.quote_str("o'x\\"), "_utf8mb4 X'6F27785C'");
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
        let (sql, values) = update_cell(Kind::MySql, "s", "t", "name", Some("a'b\\"), wh).with_placeholders();
        assert_eq!(sql, "UPDATE `s`.`t` SET `name` = ? WHERE `id` = ?");
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

    /// Value conditions added and removed in any order leave the filter as if only the ones
    /// still there had been added, whatever the user's own filter.
    #[test]
    fn value_conditions_come_and_go_in_any_order() {
        let mut g = crate::testgen::Gen::new(5);
        let conds: Vec<String> = (0..6)
            .map(|i| match i % 3 {
                0 => format!("\"c{i}\" IN ('a', ') AND (')"),
                1 => format!("(\"c{i}\" NOT IN ('x') OR \"c{i}\" IS NULL)"),
                _ => format!("\"c{i}\" IS NOT NULL"),
            })
            .collect();
        for base in ["", "active", "a OR b", "(a) AND (b)", "x IN (1, 2) AND y"] {
            for _ in 0..200 {
                let mut filter = base.to_string();
                let mut model: Vec<&String> = Vec::new();
                for _ in 0..12 {
                    let c = &conds[g.below(conds.len())];
                    if let Some(i) = model.iter().position(|m| *m == c) {
                        filter = remove_condition(&filter, c).unwrap_or_else(|| panic!("{c} in {filter}"));
                        model.remove(i);
                    } else {
                        filter = and(&filter, c);
                        model.push(c);
                    }
                    let expected = model.iter().fold(base.to_string(), |f, c| and(&f, c));
                    assert_eq!(filter, expected);
                }
            }
        }
    }

    #[test]
    fn editor_results_match_their_table_only_column_for_column() {
        use SelectItem::{All, Column, Other};
        let items = |s: &str| select_items(s).unwrap();
        assert_eq!(items("SELECT * FROM t"), [All]);
        assert_eq!(items("select c.*, id FROM t c"), [All, Column]);
        assert_eq!(
            items("SELECT id, c.\"Name\", s.t.x, id AS id, ID as id FROM t"),
            [Column; 5]
        );
        assert_eq!(
            items("SELECT name AS id, id + 1 AS id, count(*) AS n, 'a, b', f(a, b), id::text FROM t"),
            [Other; 6]
        );
        assert_eq!(
            items("SELECT \"id\" AS id, id \"ID\", id name FROM t"),
            [Other; 3]
        );
        for unreadable in [
            "SELECT 'a\\', id FROM t",
            "SELECT id -- x\n, name FROM t",
            "SELECT 1",
        ] {
            assert_eq!(select_items(unreadable), None, "{unreadable}");
        }

        let table: Vec<String> = ["id", "name", "active"].map(String::from).to_vec();
        let fits = |sql: &str, result: &[&str]| {
            let result: Vec<String> = result.iter().map(|c| c.to_string()).collect();
            result_matches_table(&select_items(sql).unwrap(), &table, &result)
        };
        assert!(fits("SELECT * FROM t", &["id", "name", "active"]));
        assert!(fits("SELECT id, name FROM t", &["id", "name"]));
        assert!(fits(
            "SELECT id, name, upper(name) AS shout FROM t",
            &["id", "name", "shout"]
        ));
        assert!(fits("SELECT *, now() FROM t", &["id", "name", "active", "now"]));
        // Another value under a table column's name: the edits would go astray.
        assert!(!fits("SELECT name AS id, id AS name FROM t", &["id", "name"]));
        assert!(!fits("SELECT id + 1 AS id, name FROM t", &["id", "name"]));
        assert!(!fits("SELECT count(*) AS id FROM t", &["id"]));
        assert!(!fits(
            "SELECT *, upper(name) AS name FROM t",
            &["id", "name", "active", "name"]
        ));
        assert!(!fits("SELECT id::text, name FROM t", &["id", "name"]));
        // The items don't line up with what came back.
        assert!(!fits("SELECT * FROM t", &["id", "name"]));
        assert!(!fits("SELECT id FROM t", &["id", "name"]));
    }

    #[test]
    fn value_filters() {
        let k = Kind::Postgres;
        let v = |x: &str| Some(x.to_string());
        let sql = distinct_values(k, "public", "customers", "\"state\"", "active", "s");
        assert_eq!(
            sql,
            "SELECT \"state\", count(*) FROM \"public\".\"customers\" WHERE ((active) AND CAST(\"state\" AS TEXT) ILIKE '%s%' ESCAPE '!') \
             GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT 1001"
        );
        // Without a search the filter is still one condition.
        assert!(
            distinct_values(k, "public", "customers", "\"state\"", "a OR b", "")
                .contains(" WHERE (a OR b) GROUP BY 1")
        );
        let c = "\"state\"";
        assert_eq!(values_condition(k, c, &[v("NY"), v("CA")], &[], true), None);
        assert_eq!(
            values_condition(k, c, &[v("NY")], &[v("CA"), None], true).as_deref(),
            Some("\"state\" IN ('NY')")
        );
        // Fewer unchecked than checked: listed instead, NULL kept.
        assert_eq!(
            values_condition(k, c, &[v("NY"), v("CA"), None], &[v("TX")], true).as_deref(),
            Some("(\"state\" NOT IN ('TX') OR \"state\" IS NULL)")
        );
        assert_eq!(
            values_condition(k, c, &[v("NY"), v("CA")], &[None], true).as_deref(),
            Some("\"state\" IS NOT NULL")
        );
        // Not every value was listed: only IN is safe.
        assert_eq!(
            values_condition(k, c, &[v("NY"), v("CA"), None], &[v("TX")], false).as_deref(),
            Some("(\"state\" IN ('NY', 'CA') OR \"state\" IS NULL)")
        );
        assert_eq!(
            values_condition(k, c, &[], &[v("NY")], true).as_deref(),
            Some("1 = 0")
        );

        let cond = "\"state\" IN ('NY')";
        assert_eq!(remove_condition(cond, cond).as_deref(), Some(""));
        assert_eq!(
            remove_condition(&and("active", cond), cond).as_deref(),
            Some("active")
        );
        assert_eq!(
            remove_condition(&and("a OR b", cond), cond).as_deref(),
            Some("a OR b")
        );
        assert_eq!(
            remove_condition(&and(&and("(a) AND (b)", cond), "x"), cond).as_deref(),
            Some("((a) AND (b)) AND x")
        );
        assert_eq!(
            remove_condition(&and(cond, "x > 1"), cond).as_deref(),
            Some("x > 1")
        );
        assert_eq!(remove_condition("active", cond), None, "not there any more");
        // Three columns filtered: any of them can go, the first, the middle or the last.
        let (a, b, c) = (
            "\"a\" IN ('1')",
            "(\"b\" IN ('2') OR \"b\" IS NULL)",
            "\"c\" IS NOT NULL",
        );
        let f = and(&and(&and("active", a), b), c);
        assert_eq!(remove_condition(&f, a), Some(and(&and("active", b), c)));
        assert_eq!(remove_condition(&f, b), Some(and(&and("active", a), c)));
        assert_eq!(remove_condition(&f, c), Some(and(&and("active", a), b)));
        // `(x) AND y OR z` is `((x) AND y) OR z`: `y` isn't a condition of its own there.
        assert_eq!(remove_condition(&format!("({a}) AND y OR z"), "y"), None);
        assert_eq!(remove_condition(&format!("({a}) AND y OR z"), a), None);
        // Quotes and parentheses inside the conditions don't cut them.
        let q = "\"d\" IN (')', '(x) AND y')";
        let f = and(&and(q, a), "y");
        assert_eq!(remove_condition(&f, a), Some(and(q, "y")));
        assert_eq!(remove_condition(&f, q), Some(and(a, "y")));
    }

    #[test]
    fn foreign_key_picker_queries() {
        let fk = FkRef {
            schema: "public".into(),
            table: "customers".into(),
            column: "id".into(),
        };
        let k = Kind::Postgres;
        assert_eq!(
            fk_options(k, &fk, Some("name"), true, true, ""),
            "SELECT \"id\", \"name\" FROM \"public\".\"customers\" ORDER BY \"name\" LIMIT 10001"
        );
        assert_eq!(
            fk_options(k, &fk, Some("name"), true, false, "42"),
            "SELECT \"id\", \"name\" FROM \"public\".\"customers\" \
             WHERE CAST(\"name\" AS TEXT) ILIKE '%42%' ESCAPE '!' OR \"id\" = 42 LIMIT 50"
        );
        // Text that isn't a number never meets a numeric key (it would be an error).
        assert!(!fk_options(k, &fk, Some("name"), true, false, "mary").contains("\"id\" ="));
        let my = fk_options(Kind::MySql, &fk, None, false, false, "a'b");
        assert!(
            my.starts_with(
                "SELECT /*+ MAX_EXECUTION_TIME(2000) */ `id`, NULL FROM `public`.`customers` WHERE"
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
                &cols(&[("id", "integer"), ("email", "text"), ("Name", "text")]),
                "id"
            )
            .as_deref(),
            Some("Name")
        );
        assert_eq!(
            fk_label(
                &cols(&[("id", "integer"), ("total", "numeric"), ("note", "varchar(20)")]),
                "id"
            )
            .as_deref(),
            Some("note")
        );
        assert_eq!(
            fk_label(&cols(&[("id", "integer"), ("total", "numeric")]), "id"),
            None
        );
    }
}
