//! SQL editor history, kept per connection in `history.json` next to the saved connections.

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::store::{self, Loaded, StoreError};

/// Oldest entries are dropped past this.
const MAX_ENTRIES: usize = 3_000;

/// Bigger SQL (a pasted dump, a long script) isn't kept: every save copies the whole history
/// on the UI thread, and the history list shows only a line of each anyway.
const MAX_SQL_BYTES: usize = 64 * 1024;

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

/// Saves the history on its own thread so running a query never waits on the disk. What it is
/// sent are changes (add this entry, remove that one), applied to the file as it is when written:
/// another window's entries are never dropped, and an entry run in both stays one. After each
/// write the file comes back, so this window also lists the other's queries. The thread ends
/// with this.
pub struct Writer {
    /// `None` once `finish` ran.
    tx: Option<Sender<Op>>,
    /// After each write: how many changes sent so far it includes, and the file's entries or why
    /// it couldn't be written.
    done: Receiver<(u64, Result<Vec<Entry>, String>)>,
    /// Changes sent so far.
    sent: std::cell::Cell<u64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

enum Op {
    Push(Entry),
    Forget {
        conn: String,
        sql: String,
    },
    /// Nothing to change: just send the file back (another window may have written it).
    Reload,
}

impl Writer {
    /// The file's place is settled here, on the caller's thread (a test's own directory).
    pub fn new() -> Self {
        let path = store::path(FILE);
        let (tx, rx) = channel::<Op>();
        let (done_tx, done) = channel();
        let thread = std::thread::spawn(move || {
            let mut applied = 0;
            while let Ok(first) = rx.recv() {
                let ops: Vec<Op> = std::iter::once(first).chain(rx.try_iter()).collect();
                applied += ops.len() as u64;
                let res = match &path {
                    Ok(p) => {
                        crate::crash::guard(|| apply(p, ops).map_err(|e| e.to_string())).unwrap_or_else(Err)
                    }
                    Err(e) => Err(e.to_string()),
                };
                if done_tx.send((applied, res)).is_err() {
                    break;
                }
            }
        });
        Writer {
            tx: Some(tx),
            done,
            sent: std::cell::Cell::new(0),
            thread: Some(thread),
        }
    }

    fn send(&self, op: Op) {
        // The thread only stops once this is dropped: sending can't fail.
        if let Some(tx) = &self.tx
            && tx.send(op).is_ok()
        {
            self.sent.set(self.sent.get() + 1);
        }
    }

    /// Asks for the file again (the window got the focus back: another may have added to it).
    pub fn reload(&self) {
        self.send(Op::Reload);
    }

    /// What came back since the last call: the file's entries when they include every change
    /// sent (newer changes are still on their way, so an older list would hide them), or why a
    /// write failed (once per failure).
    pub fn take(&self) -> Option<Result<Vec<Entry>, String>> {
        let mut out = None;
        for (applied, res) in self.done.try_iter() {
            match res {
                Err(e) => return Some(Err(e)),
                Ok(list) if applied == self.sent.get() => out = Some(Ok(list)),
                Ok(_) => {}
            }
        }
        out
    }

    /// Waits for what was sent to be written (the process ending would cut the thread off), up
    /// to `limit`: a disk that hangs must not keep the window from closing.
    pub fn finish(&mut self, limit: std::time::Duration) {
        self.tx = None;
        let Some(t) = self.thread.take() else { return };
        let deadline = std::time::Instant::now() + limit;
        while !t.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if t.is_finished() {
            let _ = t.join();
        }
    }
}

/// Applies `ops` to the file under its lock and returns its entries. Queries can contain data, so
/// the file is private like the connections file. Compact JSON: this one gets long.
fn apply(path: &Path, ops: Vec<Op>) -> Result<Vec<Entry>, StoreError> {
    if ops.iter().all(|op| matches!(op, Op::Reload)) {
        return match std::fs::read_to_string(path) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        };
    }
    store::modify_at(path, store::Layout::Compact, |list: &mut Vec<Entry>| {
        for op in ops {
            match op {
                Op::Push(entry) => {
                    insert(list, entry);
                }
                Op::Forget { conn, sql } => remove(list, &conn, &sql),
                Op::Reload => {}
            }
        }
    })
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Adds an entry (most recent last) and saves. Re-running the same SQL moves it to the end
/// instead of duplicating it.
pub fn push(list: &mut Vec<Entry>, entry: Entry, writer: &Writer) {
    if insert(list, entry.clone()) {
        writer.send(Op::Push(entry));
    }
}

/// Removes an entry and saves.
pub fn forget(list: &mut Vec<Entry>, conn: &str, sql: &str, writer: &Writer) {
    remove(list, conn, sql);
    writer.send(Op::Forget {
        conn: conn.to_string(),
        sql: sql.to_string(),
    });
}

fn remove(list: &mut Vec<Entry>, conn: &str, sql: &str) {
    list.retain(|e| !(e.conn == conn && e.sql == sql));
}

/// False when the entry is too big to keep (see `MAX_SQL_BYTES`).
fn insert(list: &mut Vec<Entry>, entry: Entry) -> bool {
    if entry.sql.len() > MAX_SQL_BYTES {
        return false;
    }
    list.retain(|e| !(e.conn == entry.conn && e.sql.trim() == entry.sql.trim()));
    list.push(entry);
    if list.len() > MAX_ENTRIES {
        list.drain(..list.len() - MAX_ENTRIES);
    }
    true
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
        assert!(
            !insert(&mut list, e(&"x".repeat(MAX_SQL_BYTES + 1), 1)),
            "too big to keep"
        );
        assert_eq!(list.len(), MAX_ENTRIES);
    }

    #[test]
    fn relative_time() {
        assert_eq!(ago(100, 130), "now");
        assert_eq!(ago(0, 300), "5 min ago");
        assert_eq!(ago(0, 7200), "2 h ago");
        assert_eq!(ago(0, 3 * 86400), "3 d ago");
    }

    #[test]
    fn two_windows_share_one_history() {
        let dir = std::env::temp_dir().join(format!("tabella-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::store::use_dir_in_this_thread(dir.clone());
        let (mut a, mut b) = (Writer::new(), Writer::new());
        let (mut la, mut lb) = (Vec::new(), Vec::new());
        push(&mut la, e("select 1", 1), &a);
        push(&mut lb, e("select 2", 2), &b);
        // The same query in both windows stays one entry, the latest run.
        push(&mut lb, e("select 1", 3), &b);
        let wait = |w: &Writer| loop {
            if let Some(r) = w.take() {
                break r.unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        wait(&a);
        let seen = wait(&b);
        let sqls: Vec<&str> = seen.iter().map(|e| e.sql.as_str()).collect();
        assert_eq!(sqls, ["select 2", "select 1"]);
        assert_eq!(seen[1].at, 3);
        // A picks up b's entries when it asks again.
        a.reload();
        let seen = wait(&a);
        assert_eq!(seen.len(), 2);
        forget(&mut la, "dev", "select 2", &a);
        assert_eq!(wait(&a).len(), 1);
        a.finish(std::time::Duration::from_secs(2));
        b.finish(std::time::Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
