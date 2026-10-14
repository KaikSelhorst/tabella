//! CSV import into an existing table: the file is read as it goes and inserted in multi-row
//! batches inside one transaction, so either every row lands or none does.

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
pub fn run(conn: &mut Conn, spec: &ImportSpec) -> DbResult<u64> {
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
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(spec.delimiter)
        .has_headers(spec.has_header)
        .flexible(true)
        .from_path(&spec.path)
        .map_err(|e| DbError::Other(format!("não foi possível abrir o arquivo: {e}")))?;

    conn.query(match spec.kind {
        Kind::Postgres => "BEGIN",
        Kind::MySql => "START TRANSACTION",
    })?;
    let result = (|| {
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
        for (n, rec) in reader.byte_records().enumerate() {
            let rec = rec.map_err(|e| {
                DbError::Other(format!(
                    "linha {} do arquivo: {e}. Nada foi importado.",
                    n as u64 + first_line
                ))
            })?;
            let row = targets
                .iter()
                .map(|(i, col)| {
                    let v = rec.get(*i).map(decode).unwrap_or_default();
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
    })();
    match result {
        Ok(n) => {
            conn.query("COMMIT")?;
            Ok(n)
        }
        Err(e) => {
            let _ = conn.query("ROLLBACK");
            Err(e)
        }
    }
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
mod tests {
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
