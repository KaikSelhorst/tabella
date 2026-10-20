//! The tables opened last, per connection (`recent.json`): `Ctrl+P` lists them first.

use std::collections::BTreeMap;

use crate::store::{self, Loaded, StoreError};

/// Connection id → `(schema, table)`, the latest first.
pub type Store = BTreeMap<String, Vec<(String, String)>>;

const FILE: &str = "recent.json";
/// Tables remembered per connection.
const KEEP: usize = 12;

pub fn load() -> Loaded<Store> {
    store::load(FILE)
}

pub fn save(recent: &Store) -> Result<(), StoreError> {
    store::save(FILE, recent)
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
}
