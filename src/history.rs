//! SQL editor history, kept per connection in `history.json` next to the saved connections.

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
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

pub fn load() -> Loaded<Vec<Entry>> {
    store::load(FILE)
}

/// Saves the history on its own thread so running a query never waits on the disk. Only the
/// newest snapshot matters, so a backlog collapses to the last one. The thread ends with this.
pub struct Writer {
    tx: Sender<Vec<Entry>>,
    /// Each write's outcome: `Some` with why it failed.
    done: Receiver<Option<String>>,
}

impl Writer {
    /// The file's place is settled here, on the caller's thread (a test's own directory).
    pub fn new() -> Self {
        let path = store::path(FILE);
        let (tx, rx) = channel::<Vec<Entry>>();
        let (done_tx, done) = channel();
        std::thread::spawn(move || {
            while let Ok(mut latest) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    latest = newer;
                }
                let error = match &path {
                    Ok(p) => write(p, &latest).err().map(|e| e.to_string()),
                    Err(e) => Some(e.to_string()),
                };
                if done_tx.send(error).is_err() {
                    break;
                }
            }
        });
        Writer { tx, done }
    }

    fn save(&self, list: &[Entry]) {
        // The thread only stops once this is dropped: sending can't fail.
        let _ = self.tx.send(list.to_vec());
    }

    /// Why the history could not be saved, once per failure (a later success clears it).
    pub fn take_error(&self) -> Option<String> {
        self.done.try_iter().last().flatten()
    }
}

/// Queries can contain data, so the file is private like the connections file. Compact JSON:
/// this one gets long.
fn write(path: &Path, list: &[Entry]) -> Result<(), StoreError> {
    let json = serde_json::to_string(list)?;
    store::write_private(path, &json)?;
    Ok(())
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Adds an entry (most recent last) and saves. Re-running the same SQL moves it to the end
/// instead of duplicating it.
pub fn push(list: &mut Vec<Entry>, entry: Entry, writer: &Writer) {
    insert(list, entry);
    writer.save(list);
}

/// Removes an entry and saves.
pub fn forget(list: &mut Vec<Entry>, conn: &str, sql: &str, writer: &Writer) {
    list.retain(|e| !(e.conn == conn && e.sql == sql));
    writer.save(list);
}

fn insert(list: &mut Vec<Entry>, entry: Entry) {
    list.retain(|e| !(e.conn == entry.conn && e.sql.trim() == entry.sql.trim()));
    list.push(entry);
    if list.len() > MAX_ENTRIES {
        list.drain(..list.len() - MAX_ENTRIES);
    }
}

/// "now", "5 min ago", "3 h ago", "2 d ago".
pub fn ago(at: u64, now: u64) -> String {
    let s = now.saturating_sub(at);
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86400),
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
        assert_eq!(ago(100, 130), "now");
        assert_eq!(ago(0, 300), "5 min ago");
        assert_eq!(ago(0, 7200), "2 h ago");
        assert_eq!(ago(0, 3 * 86400), "3 d ago");
    }
}
