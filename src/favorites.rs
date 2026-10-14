//! Saved ("favorite") queries, kept in `favorites.json` next to the connections.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Favorite {
    pub id: String,
    pub name: String,
    pub sql: String,
    /// Connection id it belongs to; empty means every connection.
    #[serde(default)]
    pub conn: String,
}

fn path() -> Option<PathBuf> {
    crate::config::dir().map(|d| d.join("favorites.json"))
}

pub fn load() -> Vec<Favorite> {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Queries can contain data, so the file is private like the connections file.
pub fn save(list: &[Favorite]) -> Result<(), String> {
    let path = path().ok_or("diretório de configuração não encontrado")?;
    let json = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    crate::config::write_private(&path, &json).map_err(|e| e.to_string())
}

/// Favorites to show for a connection: its own first, then the shared ones, each by name.
pub fn for_conn<'a>(list: &'a [Favorite], conn: &str) -> Vec<&'a Favorite> {
    let mut out: Vec<&Favorite> = list
        .iter()
        .filter(|f| f.conn == conn || f.conn.is_empty())
        .collect();
    out.sort_by_key(|f| (f.conn.is_empty(), f.name.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fav(name: &str, conn: &str) -> Favorite {
        Favorite {
            id: name.into(),
            name: name.into(),
            sql: "select 1".into(),
            conn: conn.into(),
        }
    }

    #[test]
    fn own_first_then_shared() {
        let list = [fav("b", "dev"), fav("a", ""), fav("z", "prod"), fav("A2", "dev")];
        let names: Vec<_> = for_conn(&list, "dev").iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["A2", "b", "a"]);
    }
}
