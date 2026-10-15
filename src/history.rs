//! SQL editor history, kept per connection in `history.json` next to the saved connections.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::store::{self, Loaded, StoreError};

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

const FILE: &str = "history.json";

/// The last background write's failure, until the UI picks it up with `take_write_error`.
static WRITE_ERROR: Mutex<Option<String>> = Mutex::new(None);

pub fn load() -> Loaded<Vec<Entry>> {
    store::load(FILE)
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
                let error = write(&latest).err().map(|e| e.to_string());
                if let Ok(mut slot) = WRITE_ERROR.lock() {
                    *slot = error;
                }
            }
        });
        tx
    });
    // The receiver lives as long as the process: sending can't fail.
    let _ = tx.send(list.to_vec());
}

/// Queries can contain data, so the file is private like the connections file. Compact JSON:
/// this one gets long.
fn write(list: &[Entry]) -> Result<(), StoreError> {
    let json = serde_json::to_string(list)?;
    store::write_private(&store::path(FILE)?, &json)?;
    Ok(())
}

/// Why the history could not be saved, once per failure.
pub fn take_write_error() -> Option<String> {
    WRITE_ERROR.lock().ok()?.take()
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
