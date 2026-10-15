//! `.sql` files for the editor: the system's open/save dialogs on Windows (elsewhere the app
//! asks for a path), and reading files saved in other encodings.

use std::path::{Path, PathBuf};

/// Whether the native dialogs exist; otherwise the UI asks for a path in a text field.
pub const NATIVE_DIALOGS: bool = cfg!(windows);

#[cfg(windows)]
pub fn pick_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Abrir arquivo SQL")
        .add_filter("SQL", &["sql"])
        .add_filter("Todos os arquivos", &["*"])
        .pick_file()
}

#[cfg(not(windows))]
pub fn pick_open() -> Option<PathBuf> {
    None
}

/// A SQLite database file to open.
#[cfg(windows)]
pub fn pick_database() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Abrir banco SQLite")
        .add_filter("SQLite", &["db", "sqlite", "sqlite3", "db3"])
        .add_filter("Todos os arquivos", &["*"])
        .pick_file()
}

#[cfg(not(windows))]
pub fn pick_database() -> Option<PathBuf> {
    None
}

/// Where to create a new SQLite database file.
#[cfg(windows)]
pub fn pick_new_database() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Criar banco SQLite")
        .set_file_name("novo.db")
        .add_filter("SQLite", &["db", "sqlite"])
        .save_file()
}

#[cfg(not(windows))]
pub fn pick_new_database() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
pub fn pick_save(suggested: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Salvar arquivo SQL")
        .set_file_name(suggested)
        .add_filter("SQL", &["sql"])
        .save_file()
}

#[cfg(not(windows))]
pub fn pick_save(_suggested: &str) -> Option<PathBuf> {
    None
}

/// The file's text: UTF-8 (BOM dropped), or Windows-1252/Latin-1 as older editors save it.
pub fn read_sql(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("não foi possível abrir {}: {e}", path.display()))?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    Ok(match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    })
}

/// Writes UTF-8. A path without extension gets `.sql`.
pub fn write_sql(path: &Path, text: &str) -> Result<PathBuf, String> {
    let path = if path.extension().is_none() {
        path.with_extension("sql")
    } else {
        path.to_path_buf()
    };
    std::fs::write(&path, text).map_err(|e| format!("não foi possível salvar {}: {e}", path.display()))?;
    Ok(path)
}

/// Tab title for a file: its name.
pub fn title(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_reads_latin1() {
        let dir = std::env::temp_dir().join(format!("dblite-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let saved = write_sql(&dir.join("relatorio"), "SELECT 'ação';").unwrap();
        assert_eq!(saved.extension().unwrap(), "sql");
        assert_eq!(read_sql(&saved).unwrap(), "SELECT 'ação';");
        assert_eq!(title(&saved), "relatorio.sql");
        let old = dir.join("antigo.sql");
        std::fs::write(&old, b"SELECT 'a\xe7\xe3o';").unwrap();
        assert_eq!(read_sql(&old).unwrap(), "SELECT 'ação';");
        let _ = std::fs::remove_dir_all(dir);
    }
}
