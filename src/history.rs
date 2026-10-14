//! SQL editor history, kept per connection in `history.json` next to the saved connections.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Oldest entries are dropped past this.
const MAX_ENTRIES: usize = 3_000;

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Entry {
    pub conn: String,
    pub sql: String,
    /// Unix seconds.
    pub at: u64,
    pub ok: bool,
    pub ms: u64,
}

fn path() -> Option<PathBuf> {
    crate::config::dir().map(|d| d.join("history.json"))
}

pub fn load() -> Vec<Entry> {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Writes happen on one background thread so running a query never waits on the disk. Only the
/// newest snapshot matters, so a backlog collapses to the last one.
fn save(list: &[Entry]) {
    static WRITER: OnceLock<Sender<Vec<Entry>>> = OnceLock::new();
    let tx = WRITER.get_or_init(|| {
        let (tx, rx) = channel::<Vec<Entry>>();
        std::thread::spawn(move || {
            while let Ok(mut latest) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    latest = newer;
                }
                write(&latest);
            }
        });
        tx
    });
    let _ = tx.send(list.to_vec());
}

/// Queries can contain data, so the file is private like the connections file.
fn write(list: &[Entry]) {
    let Some(path) = path() else { return };
    if let Ok(json) = serde_json::to_string(list) {
        let _ = crate::config::write_private(&path, &json);
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Adds an entry (most recent last) and saves. Re-running the same SQL moves it to the end
/// instead of duplicating it.
pub fn push(list: &mut Vec<Entry>, entry: Entry) {
    insert(list, entry);
    save(list);
}

/// Removes an entry and saves.
pub fn forget(list: &mut Vec<Entry>, conn: &str, sql: &str) {
    list.retain(|e| !(e.conn == conn && e.sql == sql));
    save(list);
}

fn insert(list: &mut Vec<Entry>, entry: Entry) {
    list.retain(|e| !(e.conn == entry.conn && e.sql.trim() == entry.sql.trim()));
    list.push(entry);
    if list.len() > MAX_ENTRIES {
        list.drain(..list.len() - MAX_ENTRIES);
    }
}

/// "agora", "há 5 min", "há 3 h", "há 2 d".
pub fn ago(at: u64, now: u64) -> String {
    let s = now.saturating_sub(at);
    match s {
        0..60 => "agora".into(),
        60..3600 => format!("há {} min", s / 60),
        3600..86400 => format!("há {} h", s / 3600),
        _ => format!("há {} d", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(sql: &str, at: u64) -> Entry {
        Entry {
            conn: "dev".into(),
            sql: sql.into(),
            at,
            ok: true,
            ms: 1,
        }
    }

    #[test]
    fn dedupes_and_orders() {
        let mut list = vec![e("select 1", 1), e("select 2", 2)];
        insert(&mut list, e(" select 1 ", 3));
        assert_eq!(list.iter().map(|x| x.at).collect::<Vec<_>>(), [2, 3]);
        for i in 0..MAX_ENTRIES as u64 {
            insert(&mut list, e(&format!("q{i}"), 10 + i));
        }
        assert_eq!(list.len(), MAX_ENTRIES);
        assert_eq!(list.last().unwrap().at, 10 + MAX_ENTRIES as u64 - 1);
    }

    #[test]
    fn relative_time() {
        assert_eq!(ago(100, 130), "agora");
        assert_eq!(ago(0, 300), "há 5 min");
        assert_eq!(ago(0, 7200), "há 2 h");
        assert_eq!(ago(0, 3 * 86400), "há 3 d");
    }
}
