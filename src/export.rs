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
    Xlsx,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Csv, Format::Xlsx, Format::Json, Format::Sql];

    pub fn label(self) -> &'static str {
        match self {
            Format::Csv => "CSV",
            Format::Json => "JSON",
            Format::Sql => "SQL (INSERT)",
            Format::Xlsx => "Excel (.xlsx)",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Json => "json",
            Format::Sql => "sql",
            Format::Xlsx => "xlsx",
        }
    }
}

/// Excel's last row (1 048 576, the header included).
const XLSX_MAX_ROWS: u64 = 1_048_575;
/// Excel's column limit.
const XLSX_MAX_COLS: usize = 16_384;

/// Where rows go: a text file written as it goes, or a sheet saved at the end.
enum Sink {
    Text(BufWriter<File>),
    Xlsx {
        sheet: Box<rust_xlsxwriter::Worksheet>,
        path: PathBuf,
    },
}

/// Streams rows into a file in one format. Call `rows` per chunk, then `finish`.
pub struct Writer {
    out: Sink,
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
        let out = if format == Format::Xlsx {
            if first.columns.len() > XLSX_MAX_COLS {
                return Err(std::io::Error::other(
                    "Excel accepts up to 16,384 columns: export to CSV for more than that",
                ));
            }
            // Header in bold, frozen above the data. Column numbers fit in u16 from here on.
            let mut sheet = rust_xlsxwriter::Worksheet::new();
            let bold = rust_xlsxwriter::Format::new().set_bold();
            for (c, name) in first.columns.iter().enumerate() {
                sheet
                    .write_string_with_format(0, c as u16, name, &bold)
                    .map_err(xlsx_err)?;
            }
            sheet.set_freeze_panes(1, 0).map_err(xlsx_err)?;
            // Fail now, not after reading the whole table, if the file can't be created.
            File::create(path)?;
            Sink::Xlsx {
                sheet: Box::new(sheet),
                path: path.to_path_buf(),
            }
        } else {
            let mut out = BufWriter::new(File::create(path)?);
            match format {
                Format::Csv => writeln!(
                    out,
                    "{}",
                    csv_line(first.columns.iter().map(|c| Some(c.as_str())))
                )?,
                Format::Json => write!(out, "[")?,
                Format::Sql | Format::Xlsx => {}
            }
            Sink::Text(out)
        };
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
            match &mut self.out {
                Sink::Xlsx { sheet, .. } => {
                    if self.written >= XLSX_MAX_ROWS {
                        return Err(std::io::Error::other(
                            "Excel accepts up to 1,048,575 rows: export to CSV for more than that",
                        ));
                    }
                    xlsx_row(sheet, (self.written + 1) as u32, &self.kinds, row)?;
                }
                Sink::Text(out) => match self.format {
                    Format::Csv => writeln!(out, "{}", csv_line(row.iter().map(|v| v.as_deref())))?,
                    Format::Json => {
                        let sep = if self.written == 0 { "\n  " } else { ",\n  " };
                        let obj = values::json_object(&self.columns, &self.kinds, row);
                        write!(out, "{sep}{}", serde_json::to_string(&obj).unwrap_or_default())?;
                    }
                    Format::Sql => writeln!(
                        out,
                        "{}",
                        crate::sql::insert_row(self.kind, &self.table, &self.columns, row)
                    )?,
                    Format::Xlsx => {}
                },
            }
            self.written += 1;
        }
        Ok(())
    }

    /// Closes the JSON array and flushes. Returns the number of rows written.
    pub fn finish(self) -> std::io::Result<u64> {
        match self.out {
            Sink::Xlsx { mut sheet, path } => {
                sheet.autofit();
                let mut book = rust_xlsxwriter::Workbook::new();
                book.push_worksheet(*sheet);
                book.save(&path).map_err(xlsx_err)?;
            }
            Sink::Text(mut out) => {
                if self.format == Format::Json {
                    writeln!(out, "{}]", if self.written == 0 { "" } else { "\n" })?;
                }
                out.flush()?;
            }
        }
        Ok(self.written)
    }
}

/// One spreadsheet row (`r`, 0 is the header). Numbers and booleans as such, so Excel can sum
/// and filter them.
fn xlsx_row(
    sheet: &mut rust_xlsxwriter::Worksheet,
    r: u32,
    kinds: &[ValueKind],
    row: &[Option<String>],
) -> std::io::Result<()> {
    for (c, v) in row.iter().enumerate() {
        let Some(v) = v else { continue };
        let c = c as u16;
        let done = match kinds.get(c as usize) {
            Some(ValueKind::Number) => v.trim().parse::<f64>().ok().map(|n| sheet.write_number(r, c, n)),
            Some(ValueKind::Bool) => crate::sql::parse_bool(v).map(|b| sheet.write_boolean(r, c, b)),
            _ => None,
        };
        match done {
            Some(res) => res.map(|_| ()).map_err(xlsx_err)?,
            None => sheet.write_string(r, c, v).map(|_| ()).map_err(xlsx_err)?,
        }
    }
    Ok(())
}

fn xlsx_err(e: rust_xlsxwriter::XlsxError) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// RFC 4180: quote fields with separators, quotes or line breaks; NULL is an empty field.
fn csv_line<'a>(fields: impl Iterator<Item = Option<&'a str>>) -> String {
    fields
        .map(|f| match f.map(defuse_formula) {
            None => String::new(),
            Some(v) if v.contains([',', '"', '\n', '\r']) => format!("\"{}\"", v.replace('"', "\"\"")),
            Some(v) => v.into_owned(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Spreadsheets run a cell starting with `=`, `+`, `-` or `@` as a formula (which can launch
/// programs): such text gets a leading `'`, shown as plain text. Numbers like `-5` stay as they are.
fn defuse_formula(v: &str) -> std::borrow::Cow<'_, str> {
    let risky = v.starts_with(['=', '+', '-', '@', '\t', '\r']) && v.trim().parse::<f64>().is_err();
    if risky { format!("'{v}").into() } else { v.into() }
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
pub fn timestamp() -> String {
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
    let r = crate::programs::command("explorer")
        .map(|mut c| c.arg(format!("/select,{}", path.display())).spawn());
    #[cfg(not(windows))]
    let r = crate::programs::command("xdg-open").map(|mut c| c.arg(path.parent().unwrap_or(path)).spawn());
    drop(r);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> QueryResult {
        QueryResult {
            columns: vec!["id".into(), "name".into(), "ok".into()],
            rows: vec![
                vec![Some("1".into()), Some("a, \"b\"\nc".into()), Some("t".into())],
                vec![Some("2".into()), None, Some("f".into())],
            ],
            ..Default::default()
        }
    }

    fn export(format: Format) -> String {
        String::from_utf8(export_bytes(format)).unwrap()
    }

    fn export_bytes(format: Format) -> Vec<u8> {
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
                name: "name".into(),
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
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        bytes
    }

    #[test]
    fn xlsx_keeps_numbers_and_booleans_typed() {
        use std::io::Read;
        let bytes = export_bytes(Format::Xlsx);
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a real .xlsx (zip)");
        let mut sheet = String::new();
        zip.by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet)
            .unwrap();
        // id 1 as a number, ok=true as a boolean, name of row 3 (NULL) left empty.
        assert!(sheet.contains(r#"<c r="A2"><v>1</v></c>"#), "{sheet}");
        assert!(sheet.contains(r#"<c r="C2" t="b"><v>1</v></c>"#), "{sheet}");
        assert!(!sheet.contains(r#"r="B3""#), "{sheet}");
        assert!(sheet.contains("<pane"), "header row frozen");
    }

    #[test]
    fn csv_quotes_only_what_needs_it() {
        assert_eq!(export(Format::Csv), "id,name,ok\n1,\"a, \"\"b\"\"\nc\",t\n2,,f\n");
    }

    #[test]
    fn csv_cells_never_become_formulas() {
        let line = csv_line(
            ["=cmd|' /C calc'!A0", "-5", "-1.5e3", "@SUM(A1)", "+x", "a=b"]
                .into_iter()
                .map(Some),
        );
        assert_eq!(line, "'=cmd|' /C calc'!A0,-5,-1.5e3,'@SUM(A1),'+x,a=b");
    }

    #[test]
    fn json_is_a_typed_array() {
        let v: serde_json::Value = serde_json::from_str(&export(Format::Json)).unwrap();
        assert_eq!(
            v,
            serde_json::json!([
                {"id": 1, "name": "a, \"b\"\nc", "ok": true},
                {"id": 2, "name": null, "ok": false}
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
        let p = target_path("public.customers/x", Format::Csv);
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("public_customers_x_2") && name.ends_with(".csv"),
            "{name}"
        );
        assert_eq!(timestamp().len(), 15);
    }
}
