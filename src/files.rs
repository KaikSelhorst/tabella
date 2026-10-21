//! `.sql` files for the editor: the system's open/save dialogs on Windows (elsewhere the app
//! asks for a path), and reading files saved in other encodings.

use std::path::{Path, PathBuf};

/// Whether the native dialogs exist; otherwise the UI asks for a path in a text field.
pub const NATIVE_DIALOGS: bool = cfg!(windows);

#[cfg(windows)]
pub fn pick_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open SQL file")
        .add_filter("SQL", &["sql"])
        .add_filter("All files", &["*"])
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
        .set_title("Open SQLite database")
        .add_filter("SQLite", &["db", "sqlite", "sqlite3", "db3"])
        .add_filter("All files", &["*"])
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
        .set_title("Create SQLite database")
        .set_file_name("new.db")
        .add_filter("SQLite", &["db", "sqlite"])
        .save_file()
}

#[cfg(not(windows))]
pub fn pick_new_database() -> Option<PathBuf> {
    None
}

/// A connections file to import.
#[cfg(windows)]
pub fn pick_connections() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Import connections")
        .add_filter("JSON", &["json"])
        .add_filter("All files", &["*"])
        .pick_file()
}

#[cfg(not(windows))]
pub fn pick_connections() -> Option<PathBuf> {
    None
}

/// Where to write the exported connections.
#[cfg(windows)]
pub fn pick_connections_save() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Export connections")
        .set_file_name("tabella-connections.json")
        .add_filter("JSON", &["json"])
        .save_file()
}

#[cfg(not(windows))]
pub fn pick_connections_save() -> Option<PathBuf> {
    None
}

/// A backup file to restore.
#[cfg(windows)]
pub fn pick_backup() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Restore backup")
        .add_filter("Backups", &["sql", "dump", "backup"])
        .add_filter("All files", &["*"])
        .pick_file()
}

#[cfg(not(windows))]
pub fn pick_backup() -> Option<PathBuf> {
    None
}

/// Where to write a backup.
#[cfg(windows)]
pub fn pick_backup_save(suggested: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Back up database")
        .set_file_name(suggested)
        .add_filter("Backups", &["sql", "dump"])
        .save_file()
}

#[cfg(not(windows))]
pub fn pick_backup_save(_suggested: &str) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
pub fn pick_save(suggested: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Save SQL file")
        .set_file_name(suggested)
        .add_filter("SQL", &["sql"])
        .save_file()
}

#[cfg(not(windows))]
pub fn pick_save(_suggested: &str) -> Option<PathBuf> {
    None
}

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("couldn't open {}: {source}", path.display())]
    Read { path: PathBuf, source: std::io::Error },
    #[error("couldn't save {}: {source}", path.display())]
    Write { path: PathBuf, source: std::io::Error },
}

/// The file's text: UTF-8 (BOM dropped), or Windows-1252/Latin-1 as older editors save it.
pub fn read_sql(path: &Path) -> Result<String, FileError> {
    let bytes = std::fs::read(path).map_err(|source| FileError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    Ok(match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    })
}

/// Writes UTF-8. A path without extension gets `.sql`.
pub fn write_sql(path: &Path, text: &str) -> Result<PathBuf, FileError> {
    let path = if path.extension().is_none() {
        path.with_extension("sql")
    } else {
        path.to_path_buf()
    };
    match std::fs::write(&path, text) {
        Ok(()) => Ok(path),
        Err(source) => Err(FileError::Write { path, source }),
    }
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
        let dir = std::env::temp_dir().join(format!("tabella-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let saved = write_sql(&dir.join("report"), "SELECT 'café';").unwrap();
        assert_eq!(saved.extension().unwrap(), "sql");
        assert_eq!(read_sql(&saved).unwrap(), "SELECT 'café';");
        assert_eq!(title(&saved), "report.sql");
        let old = dir.join("old.sql");
        std::fs::write(&old, b"SELECT 'caf\xe9';").unwrap();
        assert_eq!(read_sql(&old).unwrap(), "SELECT 'café';");
        let _ = std::fs::remove_dir_all(dir);
    }
}
