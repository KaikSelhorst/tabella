//! Tables the user starred, per connection (`starred.json`): listed first in the sidebar.

use std::collections::BTreeMap;

use crate::store::{self, Loaded, StoreError};

/// Connection id → `(schema, table)`, in the order they were starred.
pub type Store = BTreeMap<String, Vec<(String, String)>>;

const FILE: &str = "starred.json";

pub fn load() -> Loaded<Store> {
    store::load(FILE)
}

pub fn save(starred: &mut Store) -> Result<(), StoreError> {
    store::sync(FILE, starred)
}

/// Whether the table is in a connection's list.
pub fn is_starred(list: &[(String, String)], schema: &str, table: &str) -> bool {
    list.iter().any(|(s, t)| s == schema && t == table)
}

/// Stars the table, or takes the star off. Returns whether it is starred now.
pub fn toggle(store: &mut Store, conn: &str, schema: &str, table: &str) -> bool {
    let list = store.entry(conn.to_string()).or_default();
    let before = list.len();
    list.retain(|(s, t)| !(s == schema && t == table));
    if list.len() == before {
        list.push((schema.to_string(), table.to_string()));
        true
    } else {
        if list.is_empty() {
            store.remove(conn);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starring_toggles_and_keeps_the_order() {
        let mut s = Store::new();
        assert!(toggle(&mut s, "c", "public", "orders"));
        assert!(toggle(&mut s, "c", "public", "customers"));
        assert_eq!(s["c"][0].1, "orders", "the order they were starred in");
        assert!(is_starred(&s["c"], "public", "orders"));
        assert!(!s.contains_key("d"), "per connection");
        assert!(!toggle(&mut s, "c", "public", "orders"));
        assert!(!is_starred(&s["c"], "public", "orders"));
        assert!(!toggle(&mut s, "c", "public", "customers"));
        assert!(s.is_empty(), "an empty list leaves no entry");
    }
}
