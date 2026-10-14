//! What kind of data each result column holds, inferred from the schema type or from a sample of
//! values. Drives how cells are drawn and edited, and how rows are exported.

use crate::db::{ColumnInfo, QueryResult};

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
    let sample = &res.rows[..res.rows.len().min(SAMPLE_ROWS)];
    (0..res.columns.len())
        .map(|ci| {
            value_kind(
                info[ci].map(|c| c.data_type.as_str()),
                sample.iter().map(|r| r[ci].as_deref()),
            )
        })
        .collect()
}

/// The row as a JSON object; numbers and booleans keep their JSON types.
pub fn row_json(res: &QueryResult, cols: &[ColumnInfo], row: usize) -> String {
    let kinds = column_kinds(res, cols);
    let obj = json_object(&res.columns, &kinds, &res.rows[row]);
    serde_json::to_string_pretty(&obj).unwrap_or_default()
}

/// One row as a JSON object, keyed by column name in column order.
pub fn json_object(
    columns: &[String],
    kinds: &[ValueKind],
    row: &[Option<String>],
) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::Value;
    let mut map = serde_json::Map::new();
    for (ci, name) in columns.iter().enumerate() {
        let v = match (&row[ci], kinds[ci]) {
            (None, _) => Value::Null,
            (Some(v), ValueKind::Bool) => Value::Bool(matches!(v.as_str(), "t" | "1" | "true")),
            (Some(v), ValueKind::Number) => serde_json::from_str::<serde_json::Number>(v)
                .map_or_else(|_| Value::String(v.clone()), Value::Number),
            (Some(v), _) => Value::String(v.clone()),
        };
        map.insert(name.clone(), v);
    }
    map
}

/// A rectangle of cells, inclusive: `(first_row, first_col, last_row, last_col)`.
pub type CellRange = (usize, usize, usize, usize);

/// The cells as tab-separated text, the format spreadsheets copy and paste. NULL becomes an
/// empty cell; values with tabs, line breaks or quotes are quoted ("" for a quote).
pub fn to_tsv(res: &QueryResult, (r0, c0, r1, c1): CellRange, headers: bool) -> String {
    let field = |v: &str| {
        if v.contains(['\t', '\n', '\r', '"']) {
            format!("\"{}\"", v.replace('"', "\"\""))
        } else {
            v.to_string()
        }
    };
    let mut out = String::new();
    if headers {
        let names: Vec<String> = res.columns[c0..=c1].iter().map(|c| field(c)).collect();
        out.push_str(&names.join("\t"));
        out.push('\n');
    }
    for row in &res.rows[r0..=r1] {
        let cells: Vec<String> = row[c0..=c1]
            .iter()
            .map(|v| v.as_deref().map(field).unwrap_or_default())
            .collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    // A single value copies bare, like before ranges existed.
    if r0 == r1 && c0 == c1 && !headers {
        out.pop();
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

pub fn stats(res: &QueryResult, kinds: &[ValueKind], (r0, c0, r1, c1): CellRange) -> Stats {
    let mut st = Stats {
        cells: 0,
        values: 0,
        numbers: None,
    };
    for row in &res.rows[r0..=r1] {
        for (ci, v) in row.iter().enumerate().take(c1 + 1).skip(c0) {
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

/// Most common values kept in `ColumnStats::top`.
pub const TOP_VALUES: usize = 10;

/// Stats over the rows already loaded (editor results, which have no table to ask).
pub fn local_column_stats(res: &QueryResult, ci: usize, kind: ValueKind) -> ColumnStats {
    let mut counts: std::collections::HashMap<Option<&str>, i64> = std::collections::HashMap::new();
    for row in &res.rows {
        *counts.entry(row[ci].as_deref()).or_default() += 1;
    }
    let present = || res.rows.iter().filter_map(|r| r[ci].as_deref());
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
    top.truncate(TOP_VALUES);
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
    let row = summary.rows.first().ok_or("resposta vazia")?;
    let num = |i: usize| -> i64 {
        row.get(i)
            .and_then(|v| v.as_deref())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    let total = num(0);
    Ok(ColumnStats {
        total,
        nulls: total - num(1),
        distinct: num(2),
        min: row.get(3).cloned().flatten(),
        max: row.get(4).cloned().flatten(),
        top: top
            .rows
            .iter()
            .map(|r| {
                (
                    r[0].clone(),
                    r.get(1)
                        .and_then(|v| v.as_deref())
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                )
            })
            .collect(),
    })
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

#[cfg(test)]
mod tests {
    #[test]
    fn tsv_round_trip() {
        let res = QueryResult {
            columns: vec!["id".into(), "obs".into()],
            rows: vec![
                vec![Some("1".into()), Some("a\tb \"c\"".into())],
                vec![Some("2".into()), None],
            ],
            ..Default::default()
        };
        let all = to_tsv(&res, (0, 0, 1, 1), true);
        assert_eq!(all, "id\tobs\n1\t\"a\tb \"\"c\"\"\"\n2\t\n");
        assert_eq!(parse_tsv(&all), [["id", "obs"], ["1", "a\tb \"c\""], ["2", ""]]);
        assert_eq!(to_tsv(&res, (1, 0, 1, 0), false), "2");
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
    fn selection_stats() {
        let res = QueryResult {
            columns: vec!["n".into(), "t".into()],
            rows: vec![
                vec![Some("1.5".into()), Some("a".into())],
                vec![Some("4".into()), None],
                vec![None, Some("b".into())],
            ],
            ..Default::default()
        };
        let st = stats(&res, &[ValueKind::Number, ValueKind::Text], (0, 0, 2, 1));
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
            columns: vec!["id".into(), "ativo".into(), "nome".into()],
            rows: vec![vec![Some("7".into()), Some("t".into()), None]],
            ..Default::default()
        };
        let cols = [
            ColumnInfo {
                name: "id".into(),
                data_type: "integer".into(),
                pk: true,
                nullable: true,
                has_default: false,
                references: None,
            },
            ColumnInfo {
                name: "ativo".into(),
                data_type: "boolean".into(),
                pk: false,
                nullable: true,
                has_default: false,
                references: None,
            },
            ColumnInfo {
                name: "nome".into(),
                data_type: "text".into(),
                pk: false,
                nullable: true,
                has_default: false,
                references: None,
            },
        ];
        let v: serde_json::Value = serde_json::from_str(&row_json(&res, &cols, 0)).unwrap();
        assert_eq!(v, serde_json::json!({"id": 7, "ativo": true, "nome": null}));
    }
}
