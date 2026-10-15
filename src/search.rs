//! "Search value in all tables": which tables and columns to look in, the query for each,
//! and what a match looks like.

use crate::db::{Conn, Kind, SchemaColumn, TableRef};
use crate::error::DbResult;
use crate::sql;
use crate::values::{self, ValueKind};

/// Rows fetched per table: enough to say "found" and show where.
pub const ROWS_PER_TABLE: usize = 5;
/// Tables above this many rows are skipped unless asked for: each is a full read.
pub const BIG_TABLE: i64 = 1_000_000;

#[derive(Clone, Debug, PartialEq)]
pub struct SearchTarget {
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub schema: String,
    pub table: String,
    /// Columns where the value showed up (in the rows fetched).
    pub columns: Vec<String>,
    /// Rows fetched (at most `ROWS_PER_TABLE`: "5" means "5 or more").
    pub rows: usize,
    /// WHERE that opens the table on those rows.
    pub filter: String,
}

/// Columns worth looking at for `term`: text always; numbers only for a numeric term; dates
/// only when it looks like one; never booleans or binary data. Returns the targets and how
/// many big tables were left out.
pub fn targets(
    tables: &[TableRef],
    columns: &[SchemaColumn],
    term: &str,
    include_big: bool,
) -> (Vec<SearchTarget>, usize) {
    let term = term.trim();
    let numeric = term.parse::<f64>().is_ok();
    let date_like = term.chars().next().is_some_and(|c| c.is_ascii_digit()) && term.contains(['-', ':', '/']);
    let mut skipped = 0;
    let mut out = Vec::new();
    for t in tables.iter().filter(|t| !t.is_view) {
        if !include_big && t.estimate.is_some_and(|n| n > BIG_TABLE) {
            skipped += 1;
            continue;
        }
        let cols: Vec<String> = columns
            .iter()
            .filter(|c| c.schema == t.schema && c.table == t.name)
            .filter(|c| {
                let ty = c.data_type.to_lowercase();
                if ["bytea", "blob", "binary", "bit"].iter().any(|b| ty.contains(b)) {
                    return false;
                }
                match values::value_kind(Some(&c.data_type), std::iter::empty()) {
                    ValueKind::Text => true,
                    ValueKind::Number => numeric,
                    ValueKind::Date => date_like,
                    ValueKind::Bool => false,
                }
            })
            .map(|c| c.column.clone())
            .collect();
        if !cols.is_empty() {
            out.push(SearchTarget {
                schema: t.schema.clone(),
                table: t.name.clone(),
                columns: cols,
            });
        }
    }
    (out, skipped)
}

/// The value in any of the columns, compared as text: equal, or containing it (any case).
pub fn condition(kind: Kind, columns: &[String], term: &str, exact: bool) -> String {
    if !exact {
        return sql::search_condition(kind, columns, term);
    }
    let lit = kind.quote_str(term.trim());
    let cast = |c: &String| match kind {
        Kind::Postgres | Kind::Sqlite => format!("CAST({} AS TEXT) = {lit}", kind.quote_ident(c)),
        Kind::MySql => format!("CAST({} AS CHAR) = {lit}", kind.quote_ident(c)),
    };
    format!("({})", columns.iter().map(cast).collect::<Vec<_>>().join(" OR "))
}

/// Looks in one table; `None` when the value isn't there.
pub fn search_table(
    conn: &mut Conn,
    kind: Kind,
    t: &SearchTarget,
    term: &str,
    exact: bool,
) -> DbResult<Option<SearchHit>> {
    let cond = condition(kind, &t.columns, term, exact);
    let sql = format!(
        "SELECT * FROM {} WHERE {cond} LIMIT {ROWS_PER_TABLE}",
        kind.qualified(&t.schema, &t.table)
    );
    let res = conn.query(&sql)?;
    if res.rows.is_empty() {
        return Ok(None);
    }
    // Which of the searched columns actually hold it, to point at them and open on them only.
    let needle = term.trim().to_lowercase();
    let matched: Vec<String> = t
        .columns
        .iter()
        .filter(|c| {
            res.columns.iter().position(|rc| rc == *c).is_some_and(|i| {
                res.rows.iter().any(|r| {
                    r[i].as_deref().is_some_and(|v| {
                        let v = v.to_lowercase();
                        if exact { v == needle } else { v.contains(&needle) }
                    })
                })
            })
        })
        .cloned()
        .collect();
    let columns = if matched.is_empty() {
        t.columns.clone()
    } else {
        matched
    };
    Ok(Some(SearchHit {
        schema: t.schema.clone(),
        table: t.table.clone(),
        filter: condition(kind, &columns, term, exact),
        columns,
        rows: res.rows.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str, rows: i64) -> TableRef {
        TableRef {
            schema: "public".into(),
            name: name.into(),
            is_view: false,
            estimate: Some(rows),
            search_key: name.into(),
            size: None,
        }
    }

    fn col(table: &str, name: &str, ty: &str) -> SchemaColumn {
        SchemaColumn {
            schema: "public".into(),
            table: table.into(),
            column: name.into(),
            data_type: ty.into(),
            pk: false,
        }
    }

    #[test]
    fn looks_only_where_the_value_could_be() {
        let tables = [table("customers", 1_000), table("orders", 5_000_000)];
        let cols = [
            col("customers", "id", "integer"),
            col("customers", "email", "text"),
            col("customers", "active", "boolean"),
            col("customers", "created_at", "timestamp without time zone"),
            col("customers", "photo", "bytea"),
            col("orders", "status", "text"),
        ];
        let (t, skipped) = targets(&tables, &cols, "anna@example.com", false);
        assert_eq!(skipped, 1, "the big table waits for \"include large tables\"");
        assert_eq!(t[0].columns, ["email"]);
        let (t, _) = targets(&tables, &cols, "42", true);
        assert_eq!(
            t[0].columns,
            ["id", "email"],
            "numbers search numeric columns too"
        );
        assert_eq!(t[1].table, "orders");
        let (t, _) = targets(&tables, &cols, "2024-01-02", false);
        assert_eq!(t[0].columns, ["email", "created_at"]);
    }

    #[test]
    fn compares_as_text() {
        let cols = ["a".to_string(), "b".into()];
        assert_eq!(
            condition(Kind::Postgres, &cols, " o'x ", true),
            "(CAST(\"a\" AS TEXT) = 'o''x' OR CAST(\"b\" AS TEXT) = 'o''x')"
        );
        assert!(condition(Kind::MySql, &cols, "x", false).contains("LIKE '%x%'"));
    }
}
