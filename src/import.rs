//! CSV or spreadsheet (.xlsx, .xls, .ods) import into an existing table: rows are inserted in
//! multi-row batches inside one transaction, so either every row lands or none does. A CSV is
//! read as it goes; a spreadsheet's first sheet is read whole (Excel stops at ~1M rows anyway).

use std::path::{Path, PathBuf};

use crate::db::{Conn, Kind};
use crate::error::{DbError, DbResult};
use crate::sql::{self, Stmt};

/// Rows shown in the import dialog.
pub const PREVIEW_ROWS: usize = 5;
/// Rows per INSERT. MySQL also caps the placeholders per statement (65 535).
const BATCH_ROWS: usize = 500;
const MAX_PARAMS: usize = 60_000;

/// First lines of a file, to choose the column mapping.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub delimiter: u8,
    /// Spreadsheets: the sheet read (the first one).
    pub sheet: Option<String>,
    /// Column names from the first line, or "coluna 1", "coluna 2"... without a header.
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct ImportSpec {
    pub path: PathBuf,
    pub kind: Kind,
    pub schema: String,
    pub table: String,
    pub delimiter: u8,
    pub has_header: bool,
    /// For each file column, the table column it goes into (`None` = skip it).
    pub mapping: Vec<Option<String>>,
    /// Table columns that are booleans: "sim", "true", "1"... are normalized for the engine.
    pub bool_cols: Vec<String>,
    /// Numeric table columns: "1.520,30" (Brazilian format) is read as 1520.30.
    pub number_cols: Vec<String>,
    /// Empty cells become NULL (otherwise an empty string).
    pub empty_is_null: bool,
}

/// Reads the start of the file: delimiter (`,`, `;` or tab, whichever the first line has most
/// of), header and a few rows.
pub fn preview(path: &Path, has_header: bool) -> Result<Preview, String> {
    if is_spreadsheet(path) {
        let (sheet, mut rows) = sheet_rows(path)?;
        rows.truncate(PREVIEW_ROWS + 1);
        return finish_preview(rows, has_header, 0, Some(sheet));
    }
    let bytes = read_start(path).map_err(|e| format!("não foi possível ler {}: {e}", path.display()))?;
    let text = decode(&bytes);
    let first = text.lines().next().unwrap_or("");
    let delimiter = [b',', b';', b'\t']
        .into_iter()
        .max_by_key(|d| first.bytes().filter(|b| b == d).count())
        .unwrap_or(b',');
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut records = reader.byte_records();
    let mut rows = Vec::new();
    // The sample may end mid-line: only complete records are kept (the last one may be cut).
    for rec in records.by_ref().take(PREVIEW_ROWS + 1) {
        let Ok(rec) = rec else { break };
        rows.push(rec.iter().map(decode).collect::<Vec<String>>());
    }
    finish_preview(rows, has_header, delimiter, None)
}

fn finish_preview(
    mut rows: Vec<Vec<String>>,
    has_header: bool,
    delimiter: u8,
    sheet: Option<String>,
) -> Result<Preview, String> {
    if rows.is_empty() {
        return Err("o arquivo está vazio".into());
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let headers = if has_header {
        let mut h = rows.remove(0);
        h.resize_with(width, String::new);
        h
    } else {
        (1..=width).map(|i| format!("coluna {i}")).collect()
    };
    rows.truncate(PREVIEW_ROWS);
    Ok(Preview {
        delimiter,
        sheet,
        headers,
        rows,
    })
}

/// Table column for each file column: same name ignoring case, spaces and accents-free
/// underscores ("Data de criação" ≠ "criado_em", but "Nome" = "nome", "data nasc" = "data_nasc").
pub fn auto_mapping(headers: &[String], columns: &[String]) -> Vec<Option<String>> {
    let norm = |s: &str| s.trim().to_lowercase().replace([' ', '-'], "_");
    headers
        .iter()
        .map(|h| columns.iter().find(|c| norm(c) == norm(h)).cloned())
        .collect()
}

/// Runs the import; returns the number of rows inserted.
pub fn run(conn: &mut Conn, spec: &ImportSpec, nested: bool) -> DbResult<u64> {
    let targets: Vec<(usize, String)> = spec
        .mapping
        .iter()
        .enumerate()
        .filter_map(|(i, m)| Some((i, m.clone()?)))
        .collect();
    if targets.is_empty() {
        return Err(DbError::Other(
            "nenhuma coluna do arquivo foi associada à tabela".into(),
        ));
    }
    let names: Vec<String> = targets.iter().map(|(_, c)| c.clone()).collect();
    let per_batch = (MAX_PARAMS / names.len()).clamp(1, BATCH_ROWS);
    let records: Box<dyn Iterator<Item = Result<Vec<String>, String>>> = if is_spreadsheet(&spec.path) {
        let (_, rows) = sheet_rows(&spec.path).map_err(DbError::Other)?;
        Box::new(rows.into_iter().skip(usize::from(spec.has_header)).map(Ok))
    } else {
        let reader = csv::ReaderBuilder::new()
            .delimiter(spec.delimiter)
            .has_headers(spec.has_header)
            .flexible(true)
            .from_path(&spec.path)
            .map_err(|e| DbError::Other(format!("não foi possível abrir o arquivo: {e}")))?;
        Box::new(reader.into_byte_records().map(|r| {
            r.map(|rec| rec.iter().map(decode).collect())
                .map_err(|e| e.to_string())
        }))
    };

    conn.atomically(nested, |conn| {
        let mut batch: Vec<Vec<Option<String>>> = Vec::with_capacity(per_batch);
        let mut done = 0u64;
        // Line in the file of the batch's first row, for error messages.
        let first_line = if spec.has_header { 2 } else { 1 };
        let mut flush = |batch: &mut Vec<Vec<Option<String>>>, done: &mut u64| -> DbResult<()> {
            if batch.is_empty() {
                return Ok(());
            }
            let stmt = insert_many(spec.kind, &spec.schema, &spec.table, &names, batch);
            let from = *done + first_line;
            conn.execute(&stmt).map_err(|e| {
                DbError::Other(format!(
                    "linhas {from}–{} do arquivo: {e}. Nada foi importado.",
                    from + batch.len() as u64 - 1
                ))
            })?;
            *done += batch.len() as u64;
            batch.clear();
            Ok(())
        };
        for (n, rec) in records.enumerate() {
            let rec = rec.map_err(|e| {
                DbError::Other(format!(
                    "linha {} do arquivo: {e}. Nada foi importado.",
                    n as u64 + first_line
                ))
            })?;
            let row = targets
                .iter()
                .map(|(i, col)| {
                    let v = rec.get(*i).cloned().unwrap_or_default();
                    if v.is_empty() && spec.empty_is_null {
                        return Ok(None);
                    }
                    if spec.bool_cols.contains(col) {
                        return match sql::parse_bool(&v) {
                            Some(b) => Ok(Some(sql::bool_value(spec.kind, b).to_string())),
                            None => Err(DbError::Other(format!(
                                "linha {} do arquivo, coluna {col}: \"{v}\" não é um booleano. Nada foi importado.",
                                n as u64 + first_line
                            ))),
                        };
                    }
                    if spec.number_cols.contains(col) {
                        return Ok(Some(normalize_number(&v)));
                    }
                    Ok(Some(v))
                })
                .collect::<DbResult<Vec<_>>>()?;
            batch.push(row);
            if batch.len() == per_batch {
                flush(&mut batch, &mut done)?;
            }
        }
        flush(&mut batch, &mut done)?;
        Ok(done)
    })
}

/// `INSERT INTO t (a, b) VALUES (...), (...)` with every value bound (see `sql::Stmt`).
pub fn insert_many(
    kind: Kind,
    schema: &str,
    table: &str,
    cols: &[String],
    rows: &[Vec<Option<String>>],
) -> Stmt {
    let names: Vec<String> = cols.iter().map(|c| kind.quote_ident(c)).collect();
    let mut stmt = Stmt::new(format!(
        "INSERT INTO {} ({}) VALUES ",
        kind.qualified(schema, table),
        names.join(", ")
    ));
    for (r, row) in rows.iter().enumerate() {
        stmt = stmt.sql(if r == 0 { "(" } else { ", (" });
        for (i, v) in row.iter().enumerate() {
            if i > 0 {
                stmt = stmt.sql(", ");
            }
            stmt = stmt.value(v.as_deref());
        }
        stmt = stmt.sql(")");
    }
    stmt
}

/// "1.520,30" → "1520.30" and "1,520.30" → "1520.30": whichever separator comes last is the
/// decimal one. "1520,3" → "1520.3". Anything else is left for the database to judge.
pub fn normalize_number(v: &str) -> String {
    let v = v.trim();
    let (comma, dot) = (v.rfind(','), v.rfind('.'));
    match (comma, dot) {
        (Some(c), Some(d)) if c > d => v.replace('.', "").replace(',', "."),
        (Some(_), Some(_)) => v.replace(',', ""),
        (Some(_), None) if v.matches(',').count() == 1 => v.replace(',', "."),
        _ => v.to_string(),
    }
}

/// .xlsx, .xlsm, .xls or .ods, by its extension.
pub fn is_spreadsheet(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["xlsx", "xlsm", "xls", "ods"].contains(&e.to_lowercase().as_str()))
}

/// The first sheet's name and its cells as text: numbers as typed (`42`, `1520.3`), dates as
/// `2026-10-14` / `2026-10-14 13:05:00`, booleans as `true`/`false`, errors and blanks empty.
pub fn sheet_rows(path: &Path) -> Result<(String, Vec<Vec<String>>), String> {
    use calamine::{Data, Reader};
    let mut book =
        calamine::open_workbook_auto(path).map_err(|e| format!("não foi possível ler a planilha: {e}"))?;
    let name = book
        .sheet_names()
        .first()
        .cloned()
        .ok_or("a planilha não tem abas")?;
    let range = book
        .worksheet_range(&name)
        .map_err(|e| format!("não foi possível ler a aba {name}: {e}"))?;
    let cell = |c: &Data| match c {
        Data::Empty | Data::Error(_) => String::new(),
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Bool(b) => b.to_string(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        Data::Float(f) => f.to_string(),
        Data::DateTime(d) => excel_date(d.as_f64()),
    };
    let rows = range.rows().map(|r| r.iter().map(cell).collect()).collect();
    Ok((name, rows))
}

/// An Excel serial date (days since 1899-12-30, time as the fraction) as ISO text.
fn excel_date(serial: f64) -> String {
    let secs = (serial * 86_400.0).round() as i64 - 25_569 * 86_400;
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
    if rem == 0 {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        )
    }
}

/// Enough of the file for the preview.
fn read_start(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)?.take(64 * 1024).read_to_end(&mut buf)?;
    Ok(buf)
}

/// UTF-8 (BOM dropped), or Latin-1/Windows-1252 as spreadsheets in Portuguese often save.
fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn file(name: &str, body: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("dblite-import-{}-{name}", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn previews_with_detected_delimiter_and_encoding() {
        // Excel in Portuguese: semicolons, Windows-1252, quoted field with the delimiter inside.
        let p = file(
            "excel.csv",
            b"Nome;Cidade;Ativo\r\nJo\xe3o;\"S\xe3o Paulo; SP\";sim\r\nAna;Recife;n\xe3o\r\n",
        );
        let pv = preview(&p, true).unwrap();
        assert_eq!(pv.delimiter, b';');
        assert_eq!(pv.headers, ["Nome", "Cidade", "Ativo"]);
        assert_eq!(pv.rows[0], ["João", "São Paulo; SP", "sim"]);
        assert_eq!(pv.rows.len(), 2);
        let pv = preview(&p, false).unwrap();
        assert_eq!(pv.headers, ["coluna 1", "coluna 2", "coluna 3"]);
        assert_eq!(pv.rows.len(), 3);
        let _ = std::fs::remove_file(p);

        let p = file("bom.csv", "\u{feff}id,nome\n1,Zé\n".as_bytes());
        assert_eq!(preview(&p, true).unwrap().headers, ["id", "nome"]);
        let _ = std::fs::remove_file(p);
    }

    /// A real .xlsx: numbers as typed, dates as ISO, booleans, accents and blanks.
    pub fn xlsx(name: &str) -> PathBuf {
        use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
        let path = std::env::temp_dir().join(format!("dblite-import-{}-{name}.xlsx", std::process::id()));
        let mut book = Workbook::new();
        let sheet = book.add_worksheet().set_name("Clientes").unwrap();
        let date = Format::new().set_num_format("yyyy-mm-dd");
        let stamp = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
        for (c, h) in ["codigo", "nome", "saldo", "ativo", "nascimento", "em"]
            .iter()
            .enumerate()
        {
            sheet.write_string(0, c as u16, *h).unwrap();
        }
        sheet.write_number(1, 0, 20.0).unwrap();
        sheet.write_string(1, 1, "João Ávila").unwrap();
        sheet.write_number(1, 2, 1520.3).unwrap();
        sheet.write_boolean(1, 3, true).unwrap();
        let d = ExcelDateTime::from_ymd(1990, 5, 17).unwrap();
        sheet.write_datetime_with_format(1, 4, &d, &date).unwrap();
        let t = ExcelDateTime::from_ymd(2026, 10, 14)
            .unwrap()
            .and_hms(13, 5, 0)
            .unwrap();
        sheet.write_datetime_with_format(1, 5, &t, &stamp).unwrap();
        sheet.write_number(2, 0, 21.0).unwrap();
        // Row 3 leaves the name and the rest blank.
        book.save(&path).unwrap();
        path
    }

    #[test]
    fn reads_spreadsheets() {
        let path = xlsx("unit");
        let (sheet, rows) = sheet_rows(&path).unwrap();
        assert_eq!(sheet, "Clientes");
        assert_eq!(
            rows[1],
            [
                "20",
                "João Ávila",
                "1520.3",
                "true",
                "1990-05-17",
                "2026-10-14 13:05:00"
            ]
        );
        assert_eq!(rows[2][0], "21");
        assert!(rows[2][1..].iter().all(String::is_empty), "{:?}", rows[2]);
        let pv = preview(&path, true).unwrap();
        assert_eq!(pv.sheet.as_deref(), Some("Clientes"));
        assert_eq!(pv.headers[0], "codigo");
        assert_eq!(pv.rows.len(), 2);
        // Numbers from the sheet are already plain: "1520.3" stays 1520.3.
        assert_eq!(normalize_number("1520.3"), "1520.3");
        assert!(is_spreadsheet(Path::new("a.XLSX")) && !is_spreadsheet(Path::new("a.csv")));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reads_brazilian_numbers() {
        assert_eq!(normalize_number("1.520,30"), "1520.30");
        assert_eq!(normalize_number("1,520.30"), "1520.30");
        assert_eq!(normalize_number(" 88,5 "), "88.5");
        assert_eq!(normalize_number("-3.25"), "-3.25");
        assert_eq!(normalize_number("12"), "12");
    }

    #[test]
    fn maps_columns_by_name() {
        let cols = ["id".to_string(), "nome".into(), "data_nasc".into()];
        let heads = ["Nome".to_string(), "Data Nasc".into(), "extra".into()];
        assert_eq!(
            auto_mapping(&heads, &cols),
            [Some("nome".to_string()), Some("data_nasc".into()), None]
        );
    }

    #[test]
    fn batches_bind_every_value() {
        let rows = vec![
            vec![Some("a'b".into()), None],
            vec![Some("c".into()), Some("1".into())],
        ];
        let st = insert_many(Kind::MySql, "db", "t", &["x".into(), "y".into()], &rows);
        let (sql, values) = st.with_placeholders();
        assert_eq!(sql, "INSERT INTO `db`.`t` (`x`, `y`) VALUES (?, NULL), (?, ?)");
        assert_eq!(values, ["a'b", "c", "1"]);
        let pg = insert_many(Kind::Postgres, "db", "t", &["x".into(), "y".into()], &rows);
        assert_eq!(
            pg.render(Kind::Postgres),
            "INSERT INTO \"db\".\"t\" (\"x\", \"y\") VALUES ('a''b', NULL), ('c', '1')"
        );
    }
}
