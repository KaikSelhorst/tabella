//! Writing results to CSV / JSON / SQL files. Rows arrive in chunks so a whole table can be
//! exported without holding it in memory.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::db::{ColumnInfo, Kind, QueryResult, Row};
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
/// Characters an Excel cell holds; longer text is cut to it rather than failing the export.
const XLSX_MAX_CHARS: usize = 32_767;

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
            crate::store::create_private(path)?;
            Sink::Xlsx {
                sheet: Box::new(sheet),
                path: path.to_path_buf(),
            }
        } else {
            let mut out = BufWriter::new(crate::store::create_private(path)?);
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

    pub fn rows<'a>(&mut self, rows: impl IntoIterator<Item = Row<'a>>) -> std::io::Result<()> {
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
                    Format::Csv => writeln!(out, "{}", csv_line(row.iter()))?,
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
                // Into the private file made at the start (`save` would create it anew).
                book.save_to_writer(crate::store::create_private(&path)?)
                    .map_err(xlsx_err)?;
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

/// The result as text in memory, byte for byte what `Writer` would put in a file. `None` for
/// Excel, which is a binary file.
pub fn render(
    format: Format,
    kind: Kind,
    table: &str,
    res: &QueryResult,
    cols: &[ColumnInfo],
) -> Option<String> {
    let mut out = String::new();
    match format {
        Format::Csv => {
            out.push_str(&csv_line(res.columns.iter().map(|c| Some(c.as_str()))));
            out.push('\n');
            for row in &res.rows {
                out.push_str(&csv_line(row.iter()));
                out.push('\n');
            }
        }
        Format::Json => {
            let kinds = values::column_kinds(res, cols);
            out.push('[');
            for (i, row) in res.rows.iter().enumerate() {
                out.push_str(if i == 0 { "\n  " } else { ",\n  " });
                let obj = values::json_object(&res.columns, &kinds, row);
                out.push_str(&serde_json::to_string(&obj).unwrap_or_default());
            }
            out.push_str(if res.rows.is_empty() { "]\n" } else { "\n]\n" });
        }
        Format::Sql => {
            for row in &res.rows {
                out.push_str(&crate::sql::insert_row(kind, table, &res.columns, row));
                out.push('\n');
            }
        }
        Format::Xlsx => return None,
    }
    Some(out)
}

/// One spreadsheet row (`r`, 0 is the header). Numbers and booleans as such, so Excel can sum
/// and filter them.
fn xlsx_row(
    sheet: &mut rust_xlsxwriter::Worksheet,
    r: u32,
    kinds: &[ValueKind],
    row: Row,
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
            None => {
                let v = match v.char_indices().nth(XLSX_MAX_CHARS) {
                    Some((end, _)) => &v[..end],
                    None => v,
                };
                sheet.write_string(r, c, v).map(|_| ()).map_err(xlsx_err)?
            }
        }
    }
    Ok(())
}

fn xlsx_err(e: rust_xlsxwriter::XlsxError) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// RFC 4180: quote fields with separators, quotes or line breaks; NULL is an empty field. A
/// row that would be an empty line (one column, empty) is `""`: readers skip blank lines.
fn csv_line<'a>(fields: impl Iterator<Item = Option<&'a str>>) -> String {
    let line = fields
        .map(|f| match f.map(defuse_formula) {
            None => String::new(),
            Some(v) if v.contains([',', '"', '\n', '\r']) => format!("\"{}\"", v.replace('"', "\"\"")),
            Some(v) => v.into_owned(),
        })
        .collect::<Vec<_>>()
        .join(",");
    if line.is_empty() { "\"\"".into() } else { line }
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
    let secs = crate::history::now().cast_signed();
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
            ]
            .into(),
            ..Default::default()
        }
    }

    /// What goes to the clipboard is exactly what the file would hold.
    #[test]
    fn rendering_in_memory_matches_the_file_writer() {
        let res = result();
        for (format, ext) in [(Format::Csv, "csv"), (Format::Json, "json"), (Format::Sql, "sql")] {
            let path = std::env::temp_dir().join(format!("tabella-render-{}.{ext}", std::process::id()));
            let mut w = Writer::create(&path, format, Kind::Postgres, "\"public\".\"t\"", &res, &[]).unwrap();
            w.rows(&res.rows).unwrap();
            w.finish().unwrap();
            let file = std::fs::read_to_string(&path).unwrap();
            std::fs::remove_file(&path).unwrap();
            let text = render(format, Kind::Postgres, "\"public\".\"t\"", &res, &[]).unwrap();
            assert_eq!(text, file, "{format:?}");
        }
        let empty = QueryResult {
            columns: vec!["a".into()],
            ..Default::default()
        };
        assert_eq!(
            render(Format::Json, Kind::Postgres, "t", &empty, &[]).unwrap(),
            "[]\n"
        );
        assert!(render(Format::Xlsx, Kind::Postgres, "t", &res, &[]).is_none());
    }

    fn export(format: Format) -> String {
        String::from_utf8(export_bytes(format)).unwrap()
    }

    fn export_bytes(format: Format) -> Vec<u8> {
        let path = std::env::temp_dir().join(format!("tabella-export-{}-{format:?}", std::process::id()));
        let res = result();
        let cols = [
            ColumnInfo {
                enum_values: Vec::new(),
                name: "id".into(),
                data_type: "integer".into(),
                pk: true,
                nullable: false,
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
            ColumnInfo {
                enum_values: Vec::new(),
                name: "ok".into(),
                data_type: "boolean".into(),
                pk: false,
                nullable: true,
                has_default: false,
                references: None,
            },
        ];
        let mut w = Writer::create(&path, format, Kind::Postgres, "\"t\"", &res, &cols).unwrap();
        w.rows(res.rows.range(..1)).unwrap();
        w.rows(res.rows.range(1..)).unwrap();
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

    /// A result of awkward names and values (quotes, separators, line breaks, any letters,
    /// NULLs), all text columns.
    fn random_result(g: &mut crate::testgen::Gen) -> (QueryResult, Vec<ColumnInfo>) {
        let cols = 1 + g.below(4);
        // Unique even ignoring case: a number in front.
        let columns: Vec<String> = (0..cols).map(|i| format!("{i}{}", g.text_of(2))).collect();
        let mut res = QueryResult {
            columns: columns.clone(),
            ..Default::default()
        };
        for _ in 0..g.below(20) {
            let row: Vec<Option<String>> = (0..cols)
                .map(|_| (g.below(5) != 0).then(|| g.text_of(3)))
                .collect();
            res.rows.push(row.iter().map(|v| v.as_deref()));
        }
        let info = columns
            .into_iter()
            .map(|name| ColumnInfo {
                name,
                data_type: "text".into(),
                pk: false,
                nullable: true,
                has_default: false,
                references: None,
                enum_values: Vec::new(),
            })
            .collect();
        (res, info)
    }

    fn write(path: &Path, format: Format, res: &QueryResult, info: &[ColumnInfo], table: &str) -> u64 {
        let mut w = Writer::create(path, format, Kind::Sqlite, table, res, info).unwrap();
        w.rows(&res.rows).unwrap();
        w.finish().unwrap()
    }

    /// Table data is no one else's business: every format ends up readable by the user only,
    /// also over a file that was readable by others (XLSX is written twice, see `finish`).
    #[test]
    #[cfg(unix)]
    fn exported_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let mut g = crate::testgen::Gen::new(7);
        let (res, info) = random_result(&mut g);
        for format in Format::ALL {
            let path = std::env::temp_dir().join(format!(
                "tabella-export-private-{}-{format:?}",
                std::process::id()
            ));
            std::fs::write(&path, "").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            write(&path, format, &res, &info, "t");
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{format:?}");
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Exported files read back to the same values: CSV (text a spreadsheet would run as a
    /// formula gets its `'`), JSON, and SQL run into a table.
    #[test]
    fn any_values_come_back_from_the_file() {
        let mut g = crate::testgen::Gen::new(21);
        let dir = std::env::temp_dir();
        let cfg = crate::sqlite::tests::sample_db("export-any");
        let mut c = crate::db::Conn::connect(&cfg, None).unwrap();
        for round in 0..60 {
            let (res, info) = random_result(&mut g);
            let n = res.rows.len() as u64;
            let text = |v: Option<&str>| v.map_or(String::new(), |v| defuse_formula(v).into_owned());

            let path = dir.join(format!("tabella-export-any-{}.csv", std::process::id()));
            assert_eq!(write(&path, Format::Csv, &res, &info, "t"), n);
            let mut rd = csv::ReaderBuilder::new()
                .has_headers(false)
                .from_path(&path)
                .unwrap();
            let back: Vec<Vec<String>> = rd
                .records()
                .map(|r| r.unwrap().iter().map(str::to_owned).collect())
                .collect();
            let header: Vec<String> = res.columns.iter().map(|c| text(Some(c))).collect();
            assert_eq!(back[0], header, "round {round}");
            for (i, row) in res.rows.iter().enumerate() {
                assert_eq!(
                    back[i + 1],
                    row.iter().map(text).collect::<Vec<_>>(),
                    "round {round}, row {i}"
                );
            }

            let path = dir.join(format!("tabella-export-any-{}.json", std::process::id()));
            write(&path, Format::Json, &res, &info, "t");
            let back: Vec<serde_json::Map<String, serde_json::Value>> =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(back.len() as u64, n);
            for (i, row) in res.rows.iter().enumerate() {
                for (col, v) in res.columns.iter().zip(row.iter()) {
                    let want = v.map_or(serde_json::Value::Null, |v| v.into());
                    assert_eq!(back[i][col], want, "round {round}, row {i}, {col:?}");
                }
            }

            let table = Kind::Sqlite.quote_ident(&format!("t\"{round}"));
            let cols: Vec<String> = res
                .columns
                .iter()
                .map(|c| format!("{} TEXT", Kind::Sqlite.quote_ident(c)))
                .collect();
            c.query(&format!("CREATE TABLE {table} ({})", cols.join(", ")))
                .unwrap();
            let path = dir.join(format!("tabella-export-any-{}.sql", std::process::id()));
            write(&path, Format::Sql, &res, &info, &table);
            // Read back the way "Run .sql file" reads it.
            let file = std::io::BufReader::new(File::open(&path).unwrap());
            for item in crate::script::ScriptReader::new(file, Kind::Sqlite) {
                let crate::script::Item::Stmt { sql, .. } = item.unwrap() else {
                    panic!("round {round}: only statements")
                };
                c.query(&sql)
                    .unwrap_or_else(|e| panic!("round {round}: {e}\n{sql}"));
            }
            let names: Vec<String> = res.columns.iter().map(|c| Kind::Sqlite.quote_ident(c)).collect();
            let back = c
                .query(&format!(
                    "SELECT {} FROM {table} ORDER BY rowid",
                    names.join(", ")
                ))
                .unwrap();
            assert_eq!(back.rows, res.rows, "round {round}");
        }
        let _ = std::fs::remove_file(&cfg.database);
    }

    /// Excel files keep the values too, and a cell past Excel's 32,767 characters doesn't
    /// stop the export (it is cut to what fits).
    #[test]
    fn any_values_come_back_from_excel() {
        let mut g = crate::testgen::Gen::new(22);
        let path = std::env::temp_dir().join(format!("tabella-export-any-{}.xlsx", std::process::id()));
        for round in 0..30 {
            let (mut res, info) = random_result(&mut g);
            if round == 0 {
                res.rows
                    .push(std::iter::repeat_n(Some("é".repeat(40_000)), res.columns.len()));
            }
            write(&path, Format::Xlsx, &res, &info, "t");
            let (_, back) = crate::import::sheet_rows(&path).unwrap();
            assert_eq!(back[0], res.columns, "round {round}");
            for (i, row) in res.rows.iter().enumerate() {
                let want: Vec<String> = row
                    .iter()
                    .map(|v| v.unwrap_or("").chars().take(XLSX_MAX_CHARS).collect())
                    .collect();
                let mut got = back.get(i + 1).cloned().unwrap_or_default();
                got.resize(want.len(), String::new());
                assert_eq!(got, want, "round {round}, row {i}");
            }
        }
        let _ = std::fs::remove_file(&path);
    }
}
