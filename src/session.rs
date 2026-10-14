//! What was open when the app closed (connection, tabs, window size), restored on the next start.
//! Passwords are not part of it: reconnecting goes through the normal secret store.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct Saved {
    /// Id of the connection that was open.
    pub conn: Option<String>,
    pub tabs: Vec<SavedTab>,
    pub active: usize,
    /// Inner window size in points.
    pub window: Option<[f32; 2]>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub enum SavedTab {
    Sql {
        text: String,
    },
    Table {
        schema: String,
        table: String,
        filter: String,
        order: Option<(String, bool)>,
    },
    Structure {
        schema: String,
        table: String,
    },
}

fn path() -> Option<PathBuf> {
    crate::config::dir().map(|d| d.join("session.json"))
}

pub fn load() -> Saved {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// SQL editors can hold data, so the file is private like the others.
pub fn save(s: &Saved) {
    let Some(path) = path() else { return };
    if let Ok(json) = serde_json::to_string_pretty(s) {
        let _ = crate::config::write_private(&path, &json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let s = Saved {
            conn: Some("abc".into()),
            tabs: vec![
                SavedTab::Sql {
                    text: "select 1".into(),
                },
                SavedTab::Table {
                    schema: "public".into(),
                    table: "t".into(),
                    filter: "id > 1".into(),
                    order: Some(("id".into(), false)),
                },
            ],
            active: 1,
            window: Some([1200.0, 800.0]),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Saved>(&json).unwrap(), s);
        // A broken file just means "nothing to restore".
        assert!(serde_json::from_str::<Saved>("{oops").is_err());
    }
}
