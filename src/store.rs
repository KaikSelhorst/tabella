//! tabella's own JSON files (connections, favorites, settings...): private to the user, written
//! atomically, and never silently replaced when they can't be read.
//!
//! Two windows of the app may be open at once, each with the files as they were when it started.
//! A save therefore doesn't write the window's whole copy over the file: under a lock, it applies
//! what this window changed since it read the file to what is there now (see `merge`), so one
//! window never undoes another's new connection, password or favorite.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("configuration directory not found")]
    NoDir,
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
thread_local! {
    static TEST_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Tests: the calling thread (and no other: tests run in parallel) keeps its files in `dir`.
/// Changing the environment instead would be seen, half-way, by every other test.
#[cfg(test)]
pub fn use_dir_in_this_thread(dir: PathBuf) {
    TEST_DIR.with(|d| *d.borrow_mut() = Some(dir));
}

/// Where tabella keeps its files. `TABELLA_CONFIG_DIR` overrides it (portable setups).
pub fn dir() -> Option<PathBuf> {
    // Tests never touch the real files: their own directory, or else one for the whole run.
    #[cfg(test)]
    return Some(
        TEST_DIR
            .with(|d| d.borrow().clone())
            .unwrap_or_else(|| std::env::temp_dir().join(format!("tabella-test-{}", std::process::id()))),
    );
    #[cfg(not(test))]
    match std::env::var_os("TABELLA_CONFIG_DIR") {
        Some(d) => Some(PathBuf::from(d)),
        None => dirs::config_dir().map(|d| migrated(&d)),
    }
}

/// `tabella` inside `base`. The app used to be called dblite: its folder is renamed the first
/// time, and kept in use if that fails (open by another program), so nothing is ever lost.
fn migrated(base: &Path) -> PathBuf {
    let dir = base.join("tabella");
    let old = base.join("dblite");
    if !dir.exists() && old.is_dir() && std::fs::rename(&old, &dir).is_err() {
        return old;
    }
    dir
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
pub fn load<T: DeserializeOwned + Serialize + Default>(name: &str) -> Loaded<T> {
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
pub fn load_at<T: DeserializeOwned + Serialize + Default>(path: &Path) -> Loaded<T> {
    let problem = match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(value) => {
                remember(path, &value);
                return Loaded { value, warning: None };
            }
            Err(e) => StoreError::Json(e),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let value = T::default();
            remember(path, &value);
            return Loaded { value, warning: None };
        }
        Err(e) => StoreError::Io(e),
    };
    // Nothing usable was read: the next save replaces the file (a copy is kept below).
    forget(path);
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

/// Saves `value` to `name` in the config directory, merged with what another window saved there
/// meanwhile; `value` becomes the merged result (see `sync_with`).
pub fn sync<T: Serialize + DeserializeOwned>(name: &str, value: &mut T) -> Result<(), StoreError> {
    sync_at(&path(name)?, value)
}

pub fn sync_at<T: Serialize + DeserializeOwned>(path: &Path, value: &mut T) -> Result<(), StoreError> {
    sync_with(path, value, Layout::Pretty, merge)
}

/// Writes `value` over whatever is in `name`: for state that belongs to one window (its tabs and
/// size), where the last one closed is the one to keep.
pub fn replace<T: Serialize + ?Sized>(name: &str, value: &T) -> Result<(), StoreError> {
    let path = path(name)?;
    let _lock = lock(&path)?;
    write_private(&path, &serde_json::to_string_pretty(value)?)?;
    remember(&path, value);
    Ok(())
}

#[derive(Clone, Copy)]
pub enum Layout {
    Pretty,
    /// For a file that gets long (the history).
    Compact,
}

/// Brings `value` and the file at `path` together, under a lock: what this window changed since
/// it last read or wrote the file is applied to what the file holds now, with `merge(base, ours,
/// theirs)`; the result is written when it differs from the file, and becomes `value`, so the
/// window also shows what another one saved. With nothing changed here it just picks up the
/// file (see `App::sync_from_disk`).
pub fn sync_with<T: Serialize + DeserializeOwned>(
    path: &Path,
    value: &mut T,
    layout: Layout,
    merge: fn(&Value, &Value, &Value) -> Value,
) -> Result<(), StoreError> {
    let ours = serde_json::to_value(&*value)?;
    let _lock = lock(path)?;
    let base = bases().get(path).cloned();
    let theirs = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    // Without both (the file was unreadable, or is new), there's nothing to merge with.
    let merged = match (&base, &theirs) {
        (Some(base), Some(theirs)) if theirs != base => merge(base, &ours, theirs),
        _ => ours,
    };
    if theirs.as_ref() != Some(&merged) {
        let json = match layout {
            Layout::Pretty => serde_json::to_string_pretty(&merged)?,
            Layout::Compact => serde_json::to_string(&merged)?,
        };
        write_private(path, &json)?;
    }
    // A file this version can't read back (written by a newer one) isn't adopted: the window
    // keeps its own, and its next save again applies only its own changes.
    if let Ok(adopted) = T::deserialize(&merged) {
        *value = adopted;
    }
    remember(path, &*value);
    Ok(())
}

/// Changes the file at `path` in place, under a lock: `f` gets what it holds now (the default
/// when there is no file yet) and the result is written and returned. For changes that are
/// operations (add this, remove that) rather than a window's whole copy. A file that can't be
/// read is left alone: the error comes back instead.
pub fn modify_at<T: Serialize + DeserializeOwned + Default>(
    path: &Path,
    layout: Layout,
    f: impl FnOnce(&mut T),
) -> Result<T, StoreError> {
    let _lock = lock(path)?;
    let mut value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(e) => return Err(e.into()),
    };
    f(&mut value);
    let json = match layout {
        Layout::Pretty => serde_json::to_string_pretty(&value)?,
        Layout::Compact => serde_json::to_string(&value)?,
    };
    write_private(path, &json)?;
    Ok(value)
}

/// The contents of each file as this window last read or wrote them: the base of the next merge.
fn bases() -> MutexGuard<'static, HashMap<PathBuf, Value>> {
    static BASES: LazyLock<Mutex<HashMap<PathBuf, Value>>> = LazyLock::new(Mutex::default);
    // A panic elsewhere while it was held leaves the map usable.
    BASES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn remember<T: Serialize + ?Sized>(path: &Path, value: &T) {
    if let Ok(v) = serde_json::to_value(value) {
        bases().insert(path.to_path_buf(), v);
    }
}

/// Tests: another window's memory of `path` (each window is its own process; tests share one).
#[cfg(test)]
pub fn last_seen_by_this_window<T: Serialize + ?Sized>(path: &Path, value: &T) {
    remember(path, value);
}

fn forget(path: &Path) {
    bases().remove(path);
}

/// Held while a file is read and rewritten, so two windows saving at once don't interleave (the
/// file itself is replaced by a rename, so the lock is on a file beside it). On a network share
/// (`TABELLA_CONFIG_DIR`) the lock is only as good as the share's support for it.
fn lock(path: &Path) -> Result<File, StoreError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(name))?;
    file.lock()?;
    Ok(file)
}

/// Three-way merge of JSON documents: what changed from `base` to `ours` applied to `theirs`.
/// Objects merge key by key. Lists merge item by item, an item known by its `"id"` when it has
/// one and by its whole value otherwise: items this window removed are removed, items it added
/// or changed are added or replaced, and the other window's are kept. Where both changed the
/// same value differently, this window's wins.
pub fn merge(base: &Value, ours: &Value, theirs: &Value) -> Value {
    if ours == base || ours == theirs {
        return theirs.clone();
    }
    if theirs == base {
        return ours.clone();
    }
    match (ours, theirs) {
        (Value::Object(o), Value::Object(t)) => {
            let empty = Map::new();
            let b = base.as_object().unwrap_or(&empty);
            Value::Object(merge_objects(b, o, t))
        }
        (Value::Array(o), Value::Array(t)) => {
            let b = base.as_array().map_or(&[][..], Vec::as_slice);
            Value::Array(merge_lists(b, o, t))
        }
        _ => ours.clone(),
    }
}

fn merge_objects(
    base: &Map<String, Value>,
    ours: &Map<String, Value>,
    theirs: &Map<String, Value>,
) -> Map<String, Value> {
    let mut out = Map::new();
    let keys = theirs
        .keys()
        .chain(ours.keys().filter(|k| !theirs.contains_key(*k)));
    for k in keys {
        let (b, o, t) = (base.get(k), ours.get(k), theirs.get(k));
        let v = if o == b {
            t
        } else if t == b {
            o
        } else {
            match (o, t) {
                (Some(o), Some(t)) => {
                    out.insert(k.clone(), merge(b.unwrap_or(&Value::Null), o, t));
                    continue;
                }
                // Removed here, changed there: removed.
                (None, _) => None,
                (o, None) => o,
            }
        };
        if let Some(v) = v {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// How a list item is told apart from the others: its `"id"`, else all of it.
fn item_key(v: &Value) -> String {
    match v.get("id") {
        Some(Value::String(id)) => format!("id:{id}"),
        _ => v.to_string(),
    }
}

fn merge_lists(base: &[Value], ours: &[Value], theirs: &[Value]) -> Vec<Value> {
    let index = |list: &[Value]| -> HashMap<String, Value> {
        list.iter().map(|v| (item_key(v), v.clone())).collect()
    };
    let (b, o) = (index(base), index(ours));
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for t in theirs {
        let k = item_key(t);
        if !seen.insert(k.clone()) {
            continue;
        }
        match (b.get(&k), o.get(&k)) {
            // Removed here.
            (Some(_), None) => {}
            (b, Some(o)) => out.push(merge(b.unwrap_or(&Value::Null), o, t)),
            // Added there.
            (None, None) => out.push(t.clone()),
        }
    }
    for v in ours {
        let k = item_key(v);
        if !seen.insert(k.clone()) {
            continue;
        }
        // Added here, or changed here after the other window removed it.
        if b.get(&k) != Some(v) {
            out.push(v.clone());
        }
    }
    out
}

/// Creates (or truncates) a file only the current user can read: mode 600 on unix, also when it
/// already existed. For files holding database contents (exports, dumps) as much as for ours.
pub fn create_private(path: &Path) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(path)?;
    #[cfg(unix)]
    {
        // `mode` only applies to new files; tighten an existing one too.
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
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
    let written = (|| {
        let mut file = create_private(&tmp)?;
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
        let dir = std::env::temp_dir().join(format!("tabella-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_old_folder_moves_to_the_new_name() {
        let base = temp_dir("migrate");
        std::fs::create_dir_all(base.join("dblite")).unwrap();
        std::fs::write(base.join("dblite/connections.json"), "[]").unwrap();
        assert_eq!(migrated(&base), base.join("tabella"));
        assert!(base.join("tabella/connections.json").exists());
        assert!(!base.join("dblite").exists());
        // Already moved, or a fresh install: just the new name.
        assert_eq!(migrated(&base), base.join("tabella"));
        let _ = std::fs::remove_dir_all(&base);
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

    /// An export over a file others could read: emptied, and only ours from then on.
    #[test]
    #[cfg(unix)]
    fn create_private_tightens_an_existing_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("private");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("out.csv");
        std::fs::write(&file, "old contents").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        create_private(&file).unwrap().write_all(b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A window's own state (its tabs): the last window closed wins outright, nothing merged in.
    #[test]
    fn replace_keeps_only_the_last_window() {
        let dir = temp_dir("replace");
        use_dir_in_this_thread(dir.clone());
        replace("session.json", &vec!["a's tab".to_string()]).unwrap();
        // b, which started before a saved, closes last.
        remember(&path("session.json").unwrap(), &Vec::<String>::new());
        replace("session.json", &vec!["b's tab".to_string()]).unwrap();
        let l: Loaded<Vec<String>> = load("session.json");
        assert_eq!(l.value, ["b's tab"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path("session.json").unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_replaces_whole_file_and_leaves_no_temp() {
        let dir = temp_dir("save");
        let file = dir.join("x.json");
        sync_at(&file, &mut vec!["one".to_string(), "two".into()]).unwrap();
        forget(&file);
        sync_at(&file, &mut vec!["three".to_string()]).unwrap();
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

    #[test]
    fn merge_keeps_both_windows_changes() {
        use serde_json::json;
        let base = json!([{"id": "a", "host": "x", "port": 1}, {"id": "b", "host": "y"}]);
        // This window changed a's host and removed b; the other changed a's port and added c.
        let ours = json!([{"id": "a", "host": "z", "port": 1}]);
        let theirs = json!([{"id": "a", "host": "x", "port": 2}, {"id": "b", "host": "y"}, {"id": "c"}]);
        assert_eq!(
            merge(&base, &ours, &theirs),
            json!([{"id": "a", "host": "z", "port": 2}, {"id": "c"}])
        );
        // Objects key by key; items without an id by their value.
        let base = json!({"zoom": 1.0, "light": false, "tags": [["s", "t1"]]});
        let ours = json!({"zoom": 1.5, "light": false, "tags": [["s", "t1"], ["s", "t2"]]});
        let theirs = json!({"zoom": 1.0, "light": true, "tags": [["s", "t3"], ["s", "t1"]]});
        assert_eq!(
            merge(&base, &ours, &theirs),
            json!({"zoom": 1.5, "light": true, "tags": [["s", "t3"], ["s", "t1"], ["s", "t2"]]})
        );
    }

    #[test]
    fn two_windows_saving_lose_nothing() {
        let dir = temp_dir("windows");
        let file = dir.join("favorites.json");
        sync_at(&file, &mut vec!["one".to_string()]).unwrap();
        // Both windows start with ["one"].
        let mut a: Vec<String> = load_at(&file).value;
        let mut b = a.clone();
        let start = a.clone();
        a.push("from a".into());
        sync_at(&file, &mut a).unwrap();
        // Each window is its own process with its own memory of the file: b's is still ["one"].
        remember(&file, &start);
        b.push("from b".into());
        b.retain(|s| s != "one");
        sync_at(&file, &mut b).unwrap();
        assert_eq!(b, ["from a", "from b"], "b now shows a's too");
        let on_disk: Vec<String> = load_at(&file).value;
        assert_eq!(on_disk, ["from a", "from b"]);
        // A, with nothing changed, picks up b's changes.
        remember(&file, &a);
        sync_at(&file, &mut a).unwrap();
        assert_eq!(a, ["from a", "from b"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_waits_for_another_window_holding_the_file() {
        let dir = temp_dir("lock");
        let file = dir.join("x.json");
        let held = lock(&file).unwrap();
        let saver = {
            let file = file.clone();
            std::thread::spawn(move || {
                crate::store::use_dir_in_this_thread(file.parent().unwrap().to_path_buf());
                modify_at(&file, Layout::Pretty, |v: &mut Vec<String>| v.push("late".into())).unwrap();
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!file.exists(), "written while another held the lock");
        drop(held);
        saver.join().unwrap();
        let on_disk: Vec<String> = load_at(&file).value;
        assert_eq!(on_disk, ["late"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
