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
    let mut map = serde_json::Map::new();
    for (ci, name) in res.columns.iter().enumerate() {
        let v = match (&res.rows[row][ci], kinds[ci]) {
            (None, _) => serde_json::Value::Null,
            (Some(v), ValueKind::Bool) => serde_json::Value::Bool(matches!(v.as_str(), "t" | "1" | "true")),
            (Some(v), ValueKind::Number) => serde_json::from_str::<serde_json::Number>(v).map_or_else(
                |_| serde_json::Value::String(v.clone()),
                serde_json::Value::Number,
            ),
            (Some(v), _) => serde_json::Value::String(v.clone()),
        };
        map.insert(name.clone(), v);
    }
    serde_json::to_string_pretty(&map).unwrap_or_default()
}

#[cfg(test)]
mod tests {
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
            },
            ColumnInfo {
                name: "ativo".into(),
                data_type: "boolean".into(),
                pk: false,
                nullable: true,
            },
            ColumnInfo {
                name: "nome".into(),
                data_type: "text".into(),
                pk: false,
                nullable: true,
            },
        ];
        let v: serde_json::Value = serde_json::from_str(&row_json(&res, &cols, 0)).unwrap();
        assert_eq!(v, serde_json::json!({"id": 7, "ativo": true, "nome": null}));
    }
}
