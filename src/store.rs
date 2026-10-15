//! dblite's own JSON files (connections, favorites, settings...): private to the user, written
//! atomically, and never silently replaced when they can't be read.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("configuration directory not found")]
    NoDir,
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

/// Where dblite keeps its files. `DBLITE_CONFIG_DIR` overrides it (tests, portable setups).
pub fn dir() -> Option<PathBuf> {
    match std::env::var_os("DBLITE_CONFIG_DIR") {
        Some(d) => Some(PathBuf::from(d)),
        None => dirs::config_dir().map(|d| d.join("dblite")),
    }
}

/// `name` inside the config directory.
pub fn path(name: &str) -> Result<PathBuf, StoreError> {
    dir().map(|d| d.join(name)).ok_or(StoreError::NoDir)
}

/// A file's contents, plus a message for the user when it existed but couldn't be used.
pub struct Loaded<T> {
    pub value: T,
    pub warning: Option<String>,
}

/// Reads `name` from the config directory; see `load_at`.
pub fn load<T: DeserializeOwned + Default>(name: &str) -> Loaded<T> {
    match path(name) {
        Ok(p) => load_at(&p),
        Err(_) => Loaded {
            value: T::default(),
            warning: None,
        },
    }
}

/// A missing file is `T::default()`. One that can't be read or parsed (corrupted, or written by
/// a newer version) also gives the default, but is first copied to `<file>.bak` and reported:
/// the next save would otherwise replace the only copy of the user's data.
pub fn load_at<T: DeserializeOwned + Default>(path: &Path) -> Loaded<T> {
    let problem = match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(value) => return Loaded { value, warning: None },
            Err(e) => StoreError::Json(e),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                value: T::default(),
                warning: None,
            };
        }
        Err(e) => StoreError::Io(e),
    };
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let bak = backup_path(path);
    let warning = match std::fs::copy(path, &bak) {
        Ok(_) => format!(
            "{name} couldn't be read ({problem}); a copy was saved to {}",
            bak.display()
        ),
        Err(e) => format!(
            "{name} couldn't be read ({problem}) or copied to {} ({e})",
            bak.display()
        ),
    };
    Loaded {
        value: T::default(),
        warning: Some(warning),
    }
}

fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// Writes `value` as indented JSON to `name` in the config directory.
pub fn save<T: Serialize + ?Sized>(name: &str, value: &T) -> Result<(), StoreError> {
    save_at(&path(name)?, value)
}

pub fn save_at<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), StoreError> {
    let json = serde_json::to_string_pretty(value)?;
    write_private(path, &json)?;
    Ok(())
}

/// Writes a file only the current user can read: created with mode 600 on unix (so it is never
/// briefly world-readable). On Windows the per-user AppData folder is already private.
///
/// The contents go to a temporary file that then replaces `path`, so a crash or a full disk
/// mid-write leaves the old file intact instead of a truncated one.
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let written = (|| {
        let mut file = opts.open(&tmp)?;
        #[cfg(unix)]
        {
            // `mode` only applies to new files; tighten a leftover temp file too.
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        // Replaces the old file in one step (on Windows too).
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dblite-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_is_default_without_warning() {
        let dir = temp_dir("missing");
        let l: Loaded<Vec<String>> = load_at(&dir.join("nothing.json"));
        assert!(l.value.is_empty());
        assert!(l.warning.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_file_is_backed_up_and_reported() {
        let dir = temp_dir("corrupt");
        let file = dir.join("favorites.json");
        std::fs::write(&file, "[{\"half a file").unwrap();
        let l: Loaded<Vec<String>> = load_at(&file);
        assert!(l.value.is_empty());
        let w = l.warning.expect("reported");
        assert!(w.contains("favorites.json"), "{w}");
        assert_eq!(
            std::fs::read_to_string(dir.join("favorites.json.bak")).unwrap(),
            "[{\"half a file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_replaces_whole_file_and_leaves_no_temp() {
        let dir = temp_dir("save");
        let file = dir.join("x.json");
        save_at(&file, &vec!["one".to_string(), "two".into()]).unwrap();
        save_at(&file, &vec!["three".to_string()]).unwrap();
        let l: Loaded<Vec<String>> = load_at(&file);
        assert_eq!(l.value, vec!["three".to_string()]);
        assert!(!dir.join("x.json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
