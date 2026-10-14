//! Writing results to CSV / JSON / SQL files. Rows arrive in chunks so a whole table can be
//! exported without holding it in memory.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::db::{ColumnInfo, Kind, QueryResult};
use crate::values::{self, ValueKind};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    Csv,
    Json,
    Sql,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Csv, Format::Json, Format::Sql];

    pub fn label(self) -> &'static str {
        match self {
            Format::Csv => "CSV",
            Format::Json => "JSON",
            Format::Sql => "SQL (INSERT)",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Json => "json",
            Format::Sql => "sql",
        }
    }
}

/// Streams rows into a file in one format. Call `rows` per chunk, then `finish`.
pub struct Writer {
    out: BufWriter<File>,
    format: Format,
    kind: Kind,
    /// Qualified table name for INSERT statements.
    table: String,
    columns: Vec<String>,
    kinds: Vec<ValueKind>,
    written: u64,
}

impl Writer {
    pub fn create(
        path: &Path,
        format: Format,
        kind: Kind,
        table: &str,
        first: &QueryResult,
        cols: &[ColumnInfo],
    ) -> std::io::Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);
        match format {
            Format::Csv => writeln!(
                out,
                "{}",
                csv_line(first.columns.iter().map(|c| Some(c.as_str())))
            )?,
            Format::Json => write!(out, "[")?,
            Format::Sql => {}
        }
        Ok(Writer {
            out,
            format,
            kind,
            table: table.to_string(),
            columns: first.columns.clone(),
            kinds: values::column_kinds(first, cols),
            written: 0,
        })
    }

    pub fn rows(&mut self, rows: &[Vec<Option<String>>]) -> std::io::Result<()> {
        for row in rows {
            match self.format {
                Format::Csv => writeln!(self.out, "{}", csv_line(row.iter().map(|v| v.as_deref())))?,
                Format::Json => {
                    let sep = if self.written == 0 { "\n  " } else { ",\n  " };
                    let obj = values::json_object(&self.columns, &self.kinds, row);
                    write!(
                        self.out,
                        "{sep}{}",
                        serde_json::to_string(&obj).unwrap_or_default()
                    )?;
                }
                Format::Sql => writeln!(
                    self.out,
                    "{}",
                    crate::sql::insert_row(self.kind, &self.table, &self.columns, row)
                )?,
            }
            self.written += 1;
        }
        Ok(())
    }

    /// Closes the JSON array and flushes. Returns the number of rows written.
    pub fn finish(mut self) -> std::io::Result<u64> {
        if self.format == Format::Json {
            writeln!(self.out, "{}]", if self.written == 0 { "" } else { "\n" })?;
        }
        self.out.flush()?;
        Ok(self.written)
    }
}

/// RFC 4180: quote fields with separators, quotes or line breaks; NULL is an empty field.
fn csv_line<'a>(fields: impl Iterator<Item = Option<&'a str>>) -> String {
    fields
        .map(|f| match f {
            None => String::new(),
            Some(v) if v.contains([',', '"', '\n', '\r']) => format!("\"{}\"", v.replace('"', "\"\"")),
            Some(v) => v.to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A new file in the Downloads folder (or home), named after the table and the current time.
pub fn target_path(base: &str, format: Format) -> PathBuf {
    let dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let safe: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{safe}_{}.{}", timestamp(), format.extension()))
}

/// `YYYYMMDD-HHMMSS` in UTC, without pulling in a date library.
fn timestamp() -> String {
    let secs = crate::history::now() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Shows the file in the system file manager.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    let r = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .spawn();
    drop(r);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> QueryResult {
        QueryResult {
            columns: vec!["id".into(), "nome".into(), "ok".into()],
            rows: vec![
                vec![Some("1".into()), Some("a, \"b\"\nc".into()), Some("t".into())],
                vec![Some("2".into()), None, Some("f".into())],
            ],
            ..Default::default()
        }
    }

    fn export(format: Format) -> String {
        let path = std::env::temp_dir().join(format!("dblite-export-{}-{format:?}", std::process::id()));
        let res = result();
        let cols = [
            ColumnInfo {
                name: "id".into(),
                data_type: "integer".into(),
                pk: true,
                nullable: false,
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
            ColumnInfo {
                name: "ok".into(),
                data_type: "boolean".into(),
                pk: false,
                nullable: true,
                has_default: false,
                references: None,
            },
        ];
        let mut w = Writer::create(&path, format, Kind::Postgres, "\"t\"", &res, &cols).unwrap();
        w.rows(&res.rows[..1]).unwrap();
        w.rows(&res.rows[1..]).unwrap();
        assert_eq!(w.finish().unwrap(), 2);
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        text
    }

    #[test]
    fn csv_quotes_only_what_needs_it() {
        assert_eq!(export(Format::Csv), "id,nome,ok\n1,\"a, \"\"b\"\"\nc\",t\n2,,f\n");
    }

    #[test]
    fn json_is_a_typed_array() {
        let v: serde_json::Value = serde_json::from_str(&export(Format::Json)).unwrap();
        assert_eq!(
            v,
            serde_json::json!([
                {"id": 1, "nome": "a, \"b\"\nc", "ok": true},
                {"id": 2, "nome": null, "ok": false}
            ])
        );
    }

    #[test]
    fn sql_has_one_insert_per_row() {
        // A value with a line break keeps it inside its string literal, so count statements.
        let sql = export(Format::Sql);
        assert_eq!(sql.matches("INSERT INTO \"t\"").count(), 2);
        assert!(sql.ends_with(");\n"));
    }

    #[test]
    fn file_names_are_safe_and_dated() {
        let p = target_path("public.clientes/x", Format::Csv);
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("public_clientes_x_2") && name.ends_with(".csv"),
            "{name}"
        );
        assert_eq!(timestamp().len(), 15);
    }
}
