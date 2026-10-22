//! The tables opened last, per connection (`recent.json`): `Ctrl+P` lists them first.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::store::{self, Loaded, StoreError};

/// Connection id → `(schema, table)`, the latest first.
pub type Store = BTreeMap<String, Vec<(String, String)>>;

const FILE: &str = "recent.json";
/// Tables remembered per connection.
const KEEP: usize = 12;

pub fn load() -> Loaded<Store> {
    store::load(FILE)
}

pub fn save(recent: &mut Store) -> Result<(), StoreError> {
    let path = store::path(FILE)?;
    store::sync_with(&path, recent, store::Layout::Pretty, merge)
}

/// Two windows' recent tables: per connection, the order of this window when it opened tables
/// since it last saw the file (it opened them last), then the other window's it doesn't have.
fn merge(base: &Value, ours: &Value, theirs: &Value) -> Value {
    let (Some(o), Some(t)) = (ours.as_object(), theirs.as_object()) else {
        return ours.clone();
    };
    let b = base.as_object();
    let mut out = t.clone();
    for (conn, list) in o {
        if b.and_then(|b| b.get(conn)) == Some(list) {
            continue;
        }
        let mut merged: Vec<Value> = list.as_array().cloned().unwrap_or_default();
        for v in t.get(conn).and_then(Value::as_array).into_iter().flatten() {
            if !merged.contains(v) {
                merged.push(v.clone());
            }
        }
        merged.truncate(KEEP);
        out.insert(conn.clone(), Value::Array(merged));
    }
    Value::Object(out)
}

/// Puts the table first. Returns whether the list changed (opening the one already first does not).
pub fn touch(store: &mut Store, conn: &str, schema: &str, table: &str) -> bool {
    let list = store.entry(conn.to_string()).or_default();
    let entry = (schema.to_string(), table.to_string());
    if list.first() == Some(&entry) {
        return false;
    }
    list.retain(|e| *e != entry);
    list.insert(0, entry);
    list.truncate(KEEP);
    true
}

/// Where the table is in the connection's list (0 = the latest).
pub fn rank(store: &Store, conn: &str, schema: &str, table: &str) -> Option<usize> {
    store
        .get(conn)?
        .iter()
        .position(|(s, t)| s == schema && t == table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_comes_first_and_the_list_is_capped() {
        let mut s = Store::new();
        assert!(touch(&mut s, "c", "public", "a"));
        assert!(touch(&mut s, "c", "public", "b"));
        assert!(!touch(&mut s, "c", "public", "b"), "already first");
        assert!(touch(&mut s, "c", "public", "a"));
        assert_eq!(rank(&s, "c", "public", "a"), Some(0));
        assert_eq!(rank(&s, "c", "public", "b"), Some(1));
        assert_eq!(rank(&s, "other", "public", "a"), None, "per connection");
        for i in 0..30 {
            touch(&mut s, "c", "public", &format!("t{i}"));
        }
        assert_eq!(s["c"].len(), KEEP);
        assert_eq!(rank(&s, "c", "public", "a"), None, "old ones fall off");
    }

    #[test]
    fn the_window_that_opened_tables_last_sets_the_order() {
        use serde_json::json;
        let base = json!({"c": [["s", "a"], ["s", "b"]]});
        let ours = json!({"c": [["s", "x"], ["s", "a"], ["s", "b"]]});
        let theirs = json!({"c": [["s", "b"], ["s", "a"]], "d": [["s", "z"]]});
        assert_eq!(
            merge(&base, &ours, &theirs),
            json!({"c": [["s", "x"], ["s", "a"], ["s", "b"]], "d": [["s", "z"]]})
        );
    }
}
