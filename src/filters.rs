//! Saved WHERE filters per table ("pedidos pendentes", "clientes de SP"), in `filters.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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

fn path() -> Option<PathBuf> {
    crate::config::dir().map(|d| d.join("filters.json"))
}

pub fn load() -> Vec<SavedFilter> {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Filters can contain data (names, documents), so the file is private.
pub fn save(list: &[SavedFilter]) -> Result<(), String> {
    let path = path().ok_or("diretório de configuração não encontrado")?;
    let json = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    crate::config::write_private(&path, &json).map_err(|e| e.to_string())
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
