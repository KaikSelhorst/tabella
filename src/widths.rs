//! Column widths set by dragging a column's edge, remembered per table in `column_widths.json`
//! so a table opens the way it was left.

use std::collections::{BTreeMap, HashMap};

use crate::store::{self, Loaded, StoreError};

/// Connection id → `schema.table` → column name → width in points.
pub type Store = BTreeMap<String, BTreeMap<String, BTreeMap<String, f32>>>;

const FILE: &str = "column_widths.json";

pub fn load() -> Loaded<Store> {
    store::load(FILE)
}

pub fn save(widths: &Store) -> Result<(), StoreError> {
    store::save(FILE, widths)
}

/// A column can't be dragged narrower than this (the grid's own minimum), and a hand-edited file
/// can't make one absurdly wide.
const LIMITS: std::ops::RangeInclusive<f32> = 40.0..=2000.0;

fn key(schema: &str, table: &str) -> String {
    format!("{schema}.{table}")
}

/// The widths remembered for this table, by column name.
pub fn for_table(store: &Store, conn: &str, schema: &str, table: &str) -> HashMap<String, f32> {
    store
        .get(conn)
        .and_then(|c| c.get(&key(schema, table)))
        .map(|t| {
            t.iter()
                .filter(|(_, w)| w.is_finite())
                .map(|(n, w)| (n.clone(), w.clamp(*LIMITS.start(), *LIMITS.end())))
                .collect()
        })
        .unwrap_or_default()
}

/// Records widths (the ones that changed) for this table. Returns whether anything did.
pub fn remember(store: &mut Store, conn: &str, schema: &str, table: &str, widths: &[(String, f32)]) -> bool {
    let table = store
        .entry(conn.to_string())
        .or_default()
        .entry(key(schema, table))
        .or_default();
    let mut changed = false;
    for (name, w) in widths {
        let w = w.clamp(*LIMITS.start(), *LIMITS.end()).round();
        if table.get(name) != Some(&w) {
            table.insert(name.clone(), w);
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_are_kept_per_connection_and_table() {
        let mut s = Store::new();
        assert!(remember(
            &mut s,
            "dev",
            "public",
            "t",
            &[("name".into(), 212.4), ("id".into(), 10.0)]
        ));
        // Nothing new: nothing to write.
        assert!(!remember(&mut s, "dev", "public", "t", &[("name".into(), 212.0)]));
        let w = for_table(&s, "dev", "public", "t");
        assert_eq!(w["name"], 212.0);
        assert_eq!(w["id"], 40.0, "never below the grid's minimum");
        assert!(for_table(&s, "prod", "public", "t").is_empty());
        assert!(for_table(&s, "dev", "public", "other").is_empty());
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Store>(&json).unwrap(), s);
    }
}
