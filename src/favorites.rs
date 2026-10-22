//! Saved ("favorite") queries, kept in `favorites.json` next to the connections.

use serde::{Deserialize, Serialize};

use crate::store::{self, Loaded, StoreError};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Favorite {
    pub id: String,
    pub name: String,
    pub sql: String,
    /// Connection id it belongs to; empty means every connection.
    #[serde(default)]
    pub conn: String,
}

const FILE: &str = "favorites.json";

pub fn load() -> Loaded<Vec<Favorite>> {
    store::load(FILE)
}

/// Queries can contain data, so the file is private like the connections file. `list` picks up
/// what another window saved (see `store::sync`).
pub fn save(list: &mut Vec<Favorite>) -> Result<(), StoreError> {
    store::sync(FILE, list)
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
