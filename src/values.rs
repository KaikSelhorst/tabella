//! What kind of data each result column holds, inferred from the schema type or from a sample of
//! values. Drives how cells are drawn and edited, and how rows are exported.

use crate::db::{ColumnInfo, QueryResult};
use std::fmt::Write as _;

/// Rows looked at to guess a column's kind when the schema type is unknown (free SQL results).
pub const SAMPLE_ROWS: usize = 60;

/// How a column's values are drawn: numbers right-aligned and tinted, booleans and dates tinted.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ValueKind {
    Number,
    Bool,
    Date,
    Text,
}

pub fn value_kind<'a>(data_type: Option<&str>, samples: impl Iterator<Item = Option<&'a str>>) -> ValueKind {
    if let Some(ty) = data_type.map(str::to_lowercase) {
        if ty.starts_with("bool") || ty == "tinyint(1)" {
            return ValueKind::Bool;
        }
        // Arrays are text like `{1,2}`, whatever their elements are.
        if ty.ends_with("[]") || ty == "array" {
            return ValueKind::Text;
        }
        if [
            "int", "numeric", "decimal", "float", "double", "real", "serial", "money",
        ]
        .iter()
        .any(|k| ty.contains(k))
        {
            return ValueKind::Number;
        }
        if ty.contains("date") || ty.contains("time") {
            return ValueKind::Date;
        }
        return ValueKind::Text;
    }
    // Free SQL results carry no types: infer from a sample of values.
    let values: Vec<&str> = samples.flatten().collect();
    if values.is_empty() {
        ValueKind::Text
    } else if values.iter().all(|v| matches!(*v, "t" | "f" | "true" | "false")) {
        ValueKind::Bool
    } else if values.iter().all(|v| v.parse::<f64>().is_ok()) {
        ValueKind::Number
    } else {
        ValueKind::Text
    }
}

/// Schema info for each result column, when known (table tabs).
pub fn column_info<'a>(res: &QueryResult, cols: &'a [ColumnInfo]) -> Vec<Option<&'a ColumnInfo>> {
    res.columns
        .iter()
        .map(|n| cols.iter().find(|c| &c.name == n))
        .collect()
}

pub fn column_kinds(res: &QueryResult, cols: &[ColumnInfo]) -> Vec<ValueKind> {
    let info = column_info(res, cols);
    let sample = res.rows.range(..res.rows.len().min(SAMPLE_ROWS));
    (0..res.columns.len())
        .map(|ci| {
            // An enum's type name says nothing about its values (`time_off_status` isn't a time).
            if info[ci].is_some_and(|c| !c.enum_values.is_empty()) {
                return ValueKind::Text;
            }
            value_kind(
                info[ci].map(|c| c.data_type.as_str()),
                sample.clone().map(|r| r.cell(ci)),
            )
        })
        .collect()
}

/// The row as a JSON object; numbers and booleans keep their JSON types.
pub fn row_json(res: &QueryResult, cols: &[ColumnInfo], row: usize) -> String {
    let kinds = column_kinds(res, cols);
    let obj = json_object(&res.columns, &kinds, res.rows.row(row));
    serde_json::to_string_pretty(&obj).unwrap_or_default()
}

/// One row as a JSON object, keyed by column name in column order.
pub fn json_object(
    columns: &[String],
    kinds: &[ValueKind],
    row: crate::db::Row,
) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::Value;
    let mut map = serde_json::Map::new();
    for (ci, name) in columns.iter().enumerate() {
        let v = match (row.cell(ci), kinds[ci]) {
            (None, _) => Value::Null,
            (Some(v), ValueKind::Bool) => Value::Bool(matches!(v, "t" | "1" | "true")),
            (Some(v), ValueKind::Number) => serde_json::from_str::<serde_json::Number>(v)
                .map_or_else(|_| Value::String(v.to_owned()), Value::Number),
            (Some(v), _) => Value::String(v.to_owned()),
        };
        map.insert(name.clone(), v);
    }
    map
}

/// Selected cells: rows `r0..=r1` of the columns `cols`, in on-screen order (hidden and pinned
/// columns change that order, so a range is a list, not `c0..=c1`).
#[derive(Clone, Debug, PartialEq)]
pub struct CellRange {
    pub r0: usize,
    pub r1: usize,
    pub cols: Vec<usize>,
}

impl CellRange {
    pub fn single(row: usize, col: usize) -> Self {
        CellRange {
            r0: row,
            r1: row,
            cols: vec![col],
        }
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        (self.r0..=self.r1).contains(&row) && self.cols.contains(&col)
    }
}

/// The cells as tab-separated text, the format spreadsheets copy and paste. NULL becomes an
/// empty cell; values with tabs, line breaks or quotes are quoted ("" for a quote).
pub fn to_tsv(res: &QueryResult, range: &CellRange, headers: bool) -> String {
    let (r0, r1) = (range.r0, range.r1);
    let field = |v: &str| {
        if v.contains(['\t', '\n', '\r', '"']) {
            format!("\"{}\"", v.replace('"', "\"\""))
        } else {
            v.to_string()
        }
    };
    let mut out = String::new();
    if headers {
        let names: Vec<String> = range.cols.iter().map(|&c| field(&res.columns[c])).collect();
        out.push_str(&names.join("\t"));
        out.push('\n');
    }
    for row in res.rows.range(r0..=r1) {
        let cells: Vec<String> = range
            .cols
            .iter()
            .map(|&c| row.cell(c).map(field).unwrap_or_default())
            .collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    // A single value copies bare, like before ranges existed.
    if r0 == r1 && range.cols.len() == 1 && !headers {
        out.pop();
    }
    out
}

/// One column's distinct values as a SQL list, `(1, 2, 3)` or `('a', 'b')`, ready to paste after
/// `IN`. NULLs are left out (they never match). Returns the text and how many values it holds.
pub fn to_in_list(
    kind: crate::db::Kind,
    res: &QueryResult,
    col: usize,
    rows: std::ops::RangeInclusive<usize>,
    value_kind: ValueKind,
) -> (String, usize) {
    let mut seen = std::collections::HashSet::new();
    let items: Vec<String> = res
        .rows
        .range(rows)
        .filter_map(|r| r.cell(col))
        .filter(|v| seen.insert(*v))
        .map(|v| {
            let plain_number = value_kind == ValueKind::Number
                && !v.is_empty()
                && v.chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
                && v.parse::<f64>().is_ok();
            if plain_number {
                v.to_string()
            } else {
                kind.quote_str(v)
            }
        })
        .collect();
    (format!("({})", items.join(", ")), items.len())
}

/// The cells as a Markdown table (issues, docs, chat): numbers right-aligned, `|` escaped,
/// line breaks as spaces, NULL as an empty cell.
pub fn to_markdown(res: &QueryResult, range: &CellRange, kinds: &[ValueKind]) -> String {
    let cell = |v: &str| v.replace('|', "\\|").replace(['\r', '\n'], " ");
    let mut out = String::new();
    let header: Vec<String> = range.cols.iter().map(|&c| cell(&res.columns[c])).collect();
    let _ = writeln!(out, "| {} |", header.join(" | "));
    let align: Vec<&str> = range
        .cols
        .iter()
        .map(|&c| {
            if kinds.get(c) == Some(&ValueKind::Number) {
                "---:"
            } else {
                "---"
            }
        })
        .collect();
    let _ = writeln!(out, "| {} |", align.join(" | "));
    for row in res.rows.range(range.r0..=range.r1) {
        let cells: Vec<String> = range
            .cols
            .iter()
            .map(|&c| row.cell(c).map(cell).unwrap_or_default())
            .collect();
        let _ = writeln!(out, "| {} |", cells.join(" | "));
    }
    out
}

/// Parses spreadsheet clipboard text back into rows of cells (inverse of `to_tsv`).
pub fn parse_tsv(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let text = text.strip_suffix('\r').unwrap_or(text);
    let mut rows = vec![Vec::new()];
    let mut cell = String::new();
    let mut chars = text.chars().peekable();
    let mut at_start = true;
    while let Some(c) = chars.next() {
        match c {
            '"' if at_start => {
                // Quoted cell: runs to the closing quote; "" is a literal quote.
                while let Some(q) = chars.next() {
                    if q == '"' {
                        if chars.peek() == Some(&'"') {
                            chars.next();
                            cell.push('"');
                        } else {
                            break;
                        }
                    } else {
                        cell.push(q);
                    }
                }
                at_start = false;
            }
            '\t' => {
                rows.last_mut().unwrap().push(std::mem::take(&mut cell));
                at_start = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                rows.last_mut().unwrap().push(std::mem::take(&mut cell));
                rows.push(Vec::new());
                at_start = true;
            }
            c => {
                cell.push(c);
                at_start = false;
            }
        }
    }
    rows.last_mut().unwrap().push(cell);
    rows
}

/// Count, sum, average, min and max of the numbers in a selection.
#[derive(Debug, PartialEq)]
pub struct Stats {
    pub cells: usize,
    /// Non-NULL cells.
    pub values: usize,
    /// `(count, sum, min, max)` of the cells that are numbers.
    pub numbers: Option<(usize, f64, f64, f64)>,
}

pub fn stats(res: &QueryResult, kinds: &[ValueKind], range: &CellRange) -> Stats {
    let mut st = Stats {
        cells: 0,
        values: 0,
        numbers: None,
    };
    for row in res.rows.range(range.r0..=range.r1) {
        for &ci in &range.cols {
            let v = row.cell(ci);
            st.cells += 1;
            let Some(v) = v else { continue };
            st.values += 1;
            if kinds.get(ci) != Some(&ValueKind::Number) {
                continue;
            }
            if let Ok(n) = v.trim().parse::<f64>() {
                let (c, s, lo, hi) = st.numbers.unwrap_or((0, 0.0, f64::INFINITY, f64::NEG_INFINITY));
                st.numbers = Some((c + 1, s + n, lo.min(n), hi.max(n)));
            }
        }
    }
    st
}

/// Profile of one column: how full it is, how varied, its range and most common values.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnStats {
    pub total: i64,
    pub nulls: i64,
    pub distinct: i64,
    /// Only for numbers and dates, where the order means something.
    pub min: Option<String>,
    pub max: Option<String>,
    /// Most common values (NULL included), most frequent first.
    pub top: Vec<(Option<String>, i64)>,
}

/// Stats over the rows already loaded (editor results, which have no table to ask).
pub fn local_column_stats(res: &QueryResult, ci: usize, kind: ValueKind) -> ColumnStats {
    let mut counts: std::collections::HashMap<Option<&str>, i64> = std::collections::HashMap::new();
    for row in &res.rows {
        *counts.entry(row.cell(ci)).or_default() += 1;
    }
    let present = || res.rows.iter().filter_map(|r| r.cell(ci));
    let (min, max) = match kind {
        ValueKind::Number => {
            let nums = || present().filter_map(|v| v.parse::<f64>().ok().map(|n| (n, v)));
            (
                nums()
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|(_, v)| v.to_string()),
                nums()
                    .max_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|(_, v)| v.to_string()),
            )
        }
        ValueKind::Date => (
            present().min().map(str::to_string),
            present().max().map(str::to_string),
        ),
        _ => (None, None),
    };
    let nulls = counts.get(&None).copied().unwrap_or(0);
    let distinct = counts.keys().filter(|k| k.is_some()).count() as i64;
    let mut top: Vec<(Option<String>, i64)> = counts
        .into_iter()
        .map(|(k, n)| (k.map(str::to_string), n))
        .collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top.truncate(crate::sql::TOP_VALUES);
    ColumnStats {
        total: res.rows.len() as i64,
        nulls,
        distinct,
        min,
        max,
        top,
    }
}

/// Reads the answers to `sql::column_stats` (summary row, then the top values).
pub fn stats_from_queries(summary: &QueryResult, top: &QueryResult) -> Result<ColumnStats, String> {
    let row = summary.rows.first().ok_or("empty response")?;
    let num = |i: usize| -> i64 { row.get(i).flatten().and_then(|v| v.parse().ok()).unwrap_or(0) };
    let total = num(0);
    Ok(ColumnStats {
        total,
        nulls: total - num(1),
        distinct: num(2),
        min: row.get(3).flatten().map(str::to_owned),
        max: row.get(4).flatten().map(str::to_owned),
        top: top
            .rows
            .iter()
            .map(|r| {
                (
                    r.cell(0).map(str::to_owned),
                    r.get(1).flatten().and_then(|v| v.parse().ok()).unwrap_or(0),
                )
            })
            .collect(),
    })
}

/// A value as the insert form shows it: booleans as true/false (Postgres sends t/f, MySQL 1/0).
pub fn display_bool_or(kind: ValueKind, v: &str) -> String {
    match (kind, crate::sql::parse_bool(v)) {
        (ValueKind::Bool, Some(b)) => b.to_string(),
        _ => v.to_string(),
    }
}

/// A number for the status line: whole numbers without decimals, others with up to 4.
pub fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{n:.0}")
    } else {
        let s = format!("{n:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// JSON laid out one item per line, indented by two. Only the whitespace between tokens
/// changes: numbers and strings stay exactly as written (`1520.30` is not turned into
/// `1520.3`, big integers keep every digit), so saving it back changes nothing but layout.
pub fn json_pretty(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    let mut depth = 0usize;
    let mut chars = text.trim().chars().peekable();
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    while let Some(c) = chars.next() {
        match c {
            '"' => copy_string(&mut out, &mut chars),
            '{' | '[' => {
                out.push(c);
                // Empty ones stay on their line: `{}`, `[]`.
                skip_ws(&mut chars);
                if chars.peek().is_some_and(|n| *n == '}' || *n == ']') {
                    out.push(chars.next().unwrap_or(' '));
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(',');
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}

/// JSON on one line, without the whitespace between tokens (strings and numbers untouched).
pub fn json_compact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => copy_string(&mut out, &mut chars),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}

/// Copies a string literal (the opening quote already read), escapes included.
fn copy_string(out: &mut String, chars: &mut std::iter::Peekable<std::str::Chars>) {
    out.push('"');
    while let Some(c) = chars.next() {
        out.push(c);
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '"' => return,
            _ => {}
        }
    }
}

fn skip_ws(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
    }
}

/// Whether `text` is JSON; if not, where it breaks (1-based line and column).
pub fn json_check(text: &str) -> Result<(), (usize, usize, String)> {
    serde_json::from_str::<serde::de::IgnoredAny>(text)
        .map(|_| ())
        .map_err(|e| (e.line(), e.column(), e.to_string()))
}

/// An object or array (what is worth formatting), as opposed to a bare string or number.
pub fn looks_like_json(text: &str) -> bool {
    let t = text.trim_start();
    (t.starts_with('{') || t.starts_with('[')) && json_check(text).is_ok()
}

/// How a row of the current result relates to the pinned one.
#[derive(Clone, Debug, PartialEq)]
pub enum RowDiff {
    /// Only in the pinned result (row there).
    Removed(usize),
    /// Only in the current result (row there).
    Added(usize),
    /// In both, with these columns (of `ResultDiff::columns`) different.
    Changed(usize, usize, Vec<usize>),
    Same(usize, usize),
}

/// The pinned result (`a`) against the current one (`b`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResultDiff {
    /// Every column: the current result's, then those only the pinned one has.
    pub columns: Vec<String>,
    /// Where each of `columns` is in `a` and in `b`.
    pub in_a: Vec<Option<usize>>,
    pub in_b: Vec<Option<usize>>,
    pub rows: Vec<RowDiff>,
    pub removed: usize,
    pub added: usize,
    pub changed: usize,
    pub same: usize,
}

impl ResultDiff {
    pub fn cell<'a>(
        &self,
        a: &'a QueryResult,
        b: &'a QueryResult,
        row: &RowDiff,
        col: usize,
    ) -> (Option<Option<&'a str>>, Option<Option<&'a str>>) {
        let at = |res: &'a QueryResult, idx: Option<usize>, r: Option<usize>| Some(res.rows.cell(r?, idx?));
        let (ra, rb) = match row {
            RowDiff::Removed(r) => (Some(*r), None),
            RowDiff::Added(r) => (None, Some(*r)),
            RowDiff::Changed(x, y, _) | RowDiff::Same(x, y) => (Some(*x), Some(*y)),
        };
        (at(a, self.in_a[col], ra), at(b, self.in_b[col], rb))
    }
}

/// Matches the rows of two results by the values of `key` (a column both have), or by
/// position without one, and says what changed. Columns only one side has are shown but
/// don't make a row "changed".
pub fn diff_results(a: &QueryResult, b: &QueryResult, key: Option<&str>) -> ResultDiff {
    let mut columns: Vec<String> = b.columns.clone();
    columns.extend(a.columns.iter().filter(|c| !b.columns.contains(c)).cloned());
    let pos = |res: &QueryResult, name: &String| res.columns.iter().position(|c| c == name);
    let in_a: Vec<Option<usize>> = columns.iter().map(|c| pos(a, c)).collect();
    let in_b: Vec<Option<usize>> = columns.iter().map(|c| pos(b, c)).collect();
    let mut d = ResultDiff {
        columns,
        in_a,
        in_b,
        ..Default::default()
    };
    let shared: Vec<(usize, usize, usize)> = (0..d.columns.len())
        .filter_map(|c| Some((c, d.in_a[c]?, d.in_b[c]?)))
        .collect();
    let compare = |ra: usize, rb: usize| -> Vec<usize> {
        shared
            .iter()
            .filter(|(_, ia, ib)| a.rows.cell(ra, *ia) != b.rows.cell(rb, *ib))
            .map(|(c, _, _)| *c)
            .collect()
    };
    // Pinned rows by key (several with the same key pair up in order).
    let key_cols = key.and_then(|k| Some((pos(a, &k.to_string())?, pos(b, &k.to_string())?)));
    let mut matched_a = vec![false; a.rows.len()];
    let mut by_key: std::collections::HashMap<Option<&str>, std::collections::VecDeque<usize>> =
        Default::default();
    if let Some((ka, _)) = key_cols {
        for (r, row) in a.rows.iter().enumerate() {
            by_key.entry(row.cell(ka)).or_default().push_back(r);
        }
    }
    for (rb, row) in b.rows.iter().enumerate() {
        let ra = match key_cols {
            Some((_, kb)) => by_key.get_mut(&row.cell(kb)).and_then(|q| q.pop_front()),
            None => (rb < a.rows.len()).then_some(rb),
        };
        let Some(ra) = ra else {
            d.added += 1;
            d.rows.push(RowDiff::Added(rb));
            continue;
        };
        matched_a[ra] = true;
        let changed = compare(ra, rb);
        if changed.is_empty() {
            d.same += 1;
            d.rows.push(RowDiff::Same(ra, rb));
        } else {
            d.changed += 1;
            d.rows.push(RowDiff::Changed(ra, rb, changed));
        }
    }
    for (ra, m) in matched_a.iter().enumerate() {
        if !m {
            d.removed += 1;
            d.rows.push(RowDiff::Removed(ra));
        }
    }
    d
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_column_becomes_an_in_list() {
        use crate::db::Kind;
        let res = QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: vec![
                vec![Some("1".into()), Some("O'Neil".into())],
                vec![Some("2".into()), None],
                vec![Some("1".into()), Some("a\\b".into())],
            ]
            .into(),
            ..Default::default()
        };
        let (ids, n) = to_in_list(Kind::Postgres, &res, 0, 0..=2, ValueKind::Number);
        assert_eq!((ids.as_str(), n), ("(1, 2)", 2), "distinct, bare numbers");
        let (names, n) = to_in_list(Kind::Postgres, &res, 1, 0..=2, ValueKind::Text);
        assert_eq!(
            (names.as_str(), n),
            ("('O''Neil', E'a\\\\b')", 2),
            "no NULL, quotes escaped"
        );
        let (my, _) = to_in_list(Kind::MySql, &res, 1, 2..=2, ValueKind::Text);
        assert_eq!(my, "('a\\\\b')");
        // A number column holding something odd keeps it quoted.
        let (odd, _) = to_in_list(Kind::Postgres, &res, 1, 0..=0, ValueKind::Number);
        assert_eq!(odd, "('O''Neil')");
    }

    #[test]
    fn tsv_round_trip() {
        let res = QueryResult {
            columns: vec!["id".into(), "note".into()],
            rows: vec![
                vec![Some("1".into()), Some("a\tb \"c\"".into())],
                vec![Some("2".into()), None],
            ]
            .into(),
            ..Default::default()
        };
        let all = to_tsv(
            &res,
            &CellRange {
                r0: 0,
                r1: 1,
                cols: vec![0, 1],
            },
            true,
        );
        assert_eq!(all, "id\tnote\n1\t\"a\tb \"\"c\"\"\"\n2\t\n");
        assert_eq!(parse_tsv(&all), [["id", "note"], ["1", "a\tb \"c\""], ["2", ""]]);
        assert_eq!(to_tsv(&res, &CellRange::single(1, 0), false), "2");
        // On-screen order, not the query's.
        assert_eq!(
            to_tsv(
                &res,
                &CellRange {
                    r0: 0,
                    r1: 0,
                    cols: vec![1, 0]
                },
                true
            ),
            "note\tid\n\"a\tb \"\"c\"\"\"\t1\n"
        );
        // Excel ends with \r\n and quotes cells with line breaks.
        assert_eq!(
            parse_tsv("x\t\"l1\nl2\"\r\ny\tz\r\n"),
            [["x", "l1\nl2"], ["y", "z"]]
        );
    }

    #[test]
    fn column_profile_from_loaded_rows() {
        let res = QueryResult {
            columns: vec!["n".into()],
            rows: [Some("3"), Some("10"), None, Some("3"), Some("2.5")]
                .iter()
                .map(|v| vec![v.map(str::to_string)])
                .collect(),
            ..Default::default()
        };
        let st = local_column_stats(&res, 0, ValueKind::Number);
        assert_eq!((st.total, st.nulls, st.distinct), (5, 1, 3));
        // Numeric order, not text order ("10" < "2.5" as text).
        assert_eq!((st.min.as_deref(), st.max.as_deref()), (Some("2.5"), Some("10")));
        assert_eq!(st.top[0], (Some("3".into()), 2));
        assert!(st.top.contains(&(None, 1)));
    }

    #[test]
    fn markdown_tables() {
        let res = QueryResult {
            columns: vec!["id".into(), "note".into()],
            rows: vec![
                vec![Some("1".into()), Some("a|b\nc".into())],
                vec![Some("2".into()), None],
            ]
            .into(),
            ..Default::default()
        };
        let all = CellRange {
            r0: 0,
            r1: 1,
            cols: vec![0, 1],
        };
        assert_eq!(
            to_markdown(&res, &all, &[ValueKind::Number, ValueKind::Text]),
            "| id | note |\n| ---: | --- |\n| 1 | a\\|b c |\n| 2 |  |\n"
        );
    }

    #[test]
    fn selection_stats() {
        let res = QueryResult {
            columns: vec!["n".into(), "t".into()],
            rows: vec![
                vec![Some("1.5".into()), Some("a".into())],
                vec![Some("4".into()), None],
                vec![None, Some("b".into())],
            ]
            .into(),
            ..Default::default()
        };
        let st = stats(
            &res,
            &[ValueKind::Number, ValueKind::Text],
            &CellRange {
                r0: 0,
                r1: 2,
                cols: vec![0, 1],
            },
        );
        assert_eq!(
            st,
            Stats {
                cells: 6,
                values: 4,
                numbers: Some((2, 5.5, 1.5, 4.0))
            }
        );
        assert_eq!(format_number(5.5), "5.5");
        assert_eq!(format_number(12.0), "12");
        assert_eq!(format_number(1.0 / 3.0), "0.3333");
    }

    use super::*;

    #[test]
    fn value_kinds() {
        assert_eq!(value_kind(Some("integer"), std::iter::empty()), ValueKind::Number);
        assert_eq!(
            value_kind(Some("timestamp without time zone"), std::iter::empty()),
            ValueKind::Date
        );
        assert_eq!(value_kind(Some("boolean"), std::iter::empty()), ValueKind::Bool);
        // Arrays of numbers aren't numbers.
        assert_eq!(value_kind(Some("int4[]"), std::iter::empty()), ValueKind::Text);
        assert_eq!(value_kind(Some("text"), std::iter::empty()), ValueKind::Text);
        assert_eq!(
            value_kind(None, [Some("1"), None, Some("2.5")].into_iter()),
            ValueKind::Number
        );
        assert_eq!(
            value_kind(None, [Some("1"), Some("x")].into_iter()),
            ValueKind::Text
        );
        assert_eq!(
            value_kind(None, [Some("t"), None, Some("f")].into_iter()),
            ValueKind::Bool
        );
    }

    #[test]
    fn json_keeps_types() {
        let res = QueryResult {
            columns: vec!["id".into(), "active".into(), "name".into()],
            rows: vec![vec![Some("7".into()), Some("t".into()), None]].into(),
            ..Default::default()
        };
        let cols = [
            ColumnInfo {
                enum_values: Vec::new(),
                name: "id".into(),
                data_type: "integer".into(),
                pk: true,
                nullable: true,
                has_default: false,
                references: None,
            },
            ColumnInfo {
                enum_values: Vec::new(),
                name: "active".into(),
                data_type: "boolean".into(),
                pk: false,
                nullable: true,
                has_default: false,
                references: None,
            },
            ColumnInfo {
                enum_values: Vec::new(),
                name: "name".into(),
                data_type: "text".into(),
                pk: false,
                nullable: true,
                has_default: false,
                references: None,
            },
        ];
        let v: serde_json::Value = serde_json::from_str(&row_json(&res, &cols, 0)).unwrap();
        assert_eq!(v, serde_json::json!({"id": 7, "active": true, "name": null}));
    }

    #[test]
    fn json_layout_keeps_every_value() {
        let src = r#"{"price":1520.30,"id":12345678901234567890,"tags":[],"o":{},"s":"a, {b} \"c\": d","l":[1, 2]}"#;
        let pretty = json_pretty(src);
        assert_eq!(
            pretty,
            "{\n  \"price\": 1520.30,\n  \"id\": 12345678901234567890,\n  \"tags\": [],\n  \"o\": {},\n  \
             \"s\": \"a, {b} \\\"c\\\": d\",\n  \"l\": [\n    1,\n    2\n  ]\n}"
        );
        assert!(json_check(&pretty).is_ok());
        assert_eq!(json_compact(&pretty), src.replace(", 2", ",2"));
        let (line, col, _) = json_check("{\n  \"a\": 1,\n  \"b\" 2\n}").unwrap_err();
        assert_eq!((line, col), (3, 7));
        assert!(looks_like_json(" [1]") && !looks_like_json("\"x\"") && !looks_like_json("{bad"));
    }

    #[test]
    fn results_compare_by_key_or_position() {
        let r = |cols: &[&str], rows: &[&[&str]]| QueryResult {
            columns: cols.iter().map(|c| c.to_string()).collect(),
            rows: rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|v| (*v != "NULL").then(|| v.to_string()))
                        .collect()
                })
                .collect(),
            ..Default::default()
        };
        let a = r(
            &["id", "name", "balance"],
            &[&["1", "anna", "10"], &["2", "beth", "20"], &["3", "cole", "30"]],
        );
        // Reordered, one changed, one gone, one new, a new column, one column dropped.
        let b = r(
            &["id", "name", "state"],
            &[&["3", "cole", "NY"], &["1", "ANNA", "CA"], &["4", "dora", "TX"]],
        );
        let d = diff_results(&a, &b, Some("id"));
        assert_eq!(d.columns, ["id", "name", "state", "balance"]);
        assert_eq!((d.removed, d.added, d.changed, d.same), (1, 1, 1, 1));
        assert_eq!(d.rows[0], RowDiff::Same(2, 0));
        assert_eq!(d.rows[1], RowDiff::Changed(0, 1, vec![1]));
        assert_eq!(d.rows[2], RowDiff::Added(2));
        assert_eq!(d.rows[3], RowDiff::Removed(1));
        let (old, new) = d.cell(&a, &b, &d.rows[1], 1);
        assert_eq!((old, new), (Some(Some("anna")), Some(Some("ANNA"))));
        // By position: row 0 against row 0, and so on.
        let p = diff_results(&a, &b, None);
        assert_eq!((p.removed, p.added, p.changed, p.same), (0, 0, 3, 0));
        // Repeated keys pair up in order; NULL keys match each other.
        let a = r(&["k", "v"], &[&["x", "1"], &["x", "2"], &["NULL", "3"]]);
        let b = r(&["k", "v"], &[&["x", "1"], &["x", "9"], &["NULL", "3"]]);
        let d = diff_results(&a, &b, Some("k"));
        assert_eq!((d.removed, d.added, d.changed, d.same), (0, 0, 1, 2));
    }
}
