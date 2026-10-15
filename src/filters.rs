//! Saved WHERE filters per table ("pending orders", "customers in NY"), in `filters.json`.

use serde::{Deserialize, Serialize};

use crate::store::{self, Loaded, StoreError};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct SavedFilter {
    pub id: String,
    pub name: String,
    pub filter: String,
    /// Connection id, schema and table it applies to.
    pub conn: String,
    pub schema: String,
    pub table: String,
}

const FILE: &str = "filters.json";

pub fn load() -> Loaded<Vec<SavedFilter>> {
    store::load(FILE)
}

/// Filters can contain data (names, documents), so the file is private.
pub fn save(list: &[SavedFilter]) -> Result<(), StoreError> {
    store::save(FILE, list)
}

/// This table's filters, by name.
pub fn for_table<'a>(list: &'a [SavedFilter], conn: &str, schema: &str, table: &str) -> Vec<&'a SavedFilter> {
    let mut out: Vec<&SavedFilter> = list
        .iter()
        .filter(|f| f.conn == conn && f.schema == schema && f.table == table)
        .collect();
    out.sort_by_key(|f| f.name.to_lowercase());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_tables_filters_by_name() {
        let f = |name: &str, table: &str| SavedFilter {
            id: name.into(),
            name: name.into(),
            filter: "x = 1".into(),
            conn: "dev".into(),
            schema: "public".into(),
            table: table.into(),
        };
        let list = [f("b", "pedidos"), f("a", "pedidos"), f("c", "clientes")];
        let names: Vec<_> = for_table(&list, "dev", "public", "pedidos")
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, ["a", "b"]);
        assert!(for_table(&list, "prod", "public", "pedidos").is_empty());
    }
}
