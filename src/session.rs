//! What was open when the app closed (connection, tabs, window size), restored on the next start.
//! Passwords are not part of it: reconnecting goes through the normal secret store.

use serde::{Deserialize, Serialize};

use crate::store::{self, Loaded, StoreError};

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct Saved {
    /// Id of the connection that was open.
    pub conn: Option<String>,
    pub tabs: Vec<SavedTab>,
    pub active: usize,
    /// Inner window size in points.
    pub window: Option<[f32; 2]>,
    /// The split view, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<SavedSplit>,
}

/// Which pane each saved tab was in, and the tab each pane showed (indices into `Saved::tabs`).
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct SavedSplit {
    /// One entry per saved tab: in the right pane.
    pub right: Vec<bool>,
    /// The selected tab of the left and the right pane.
    pub shown: [usize; 2],
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub enum SavedTab {
    Sql {
        text: String,
        /// Set when the user renamed the tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// The `.sql` file the tab edits, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<std::path::PathBuf>,
    },
    Table {
        schema: String,
        table: String,
        filter: String,
        order: Option<(String, bool)>,
        #[serde(default)]
        hidden: Vec<String>,
        #[serde(default)]
        pinned: Vec<String>,
    },
    Structure {
        schema: String,
        table: String,
    },
}

const FILE: &str = "session.json";

pub fn load() -> Loaded<Saved> {
    store::load(FILE)
}

/// SQL editors can hold data, so the file is private like the others.
pub fn save(s: &Saved) -> Result<(), StoreError> {
    store::save(FILE, s)
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
                    title: Some("Report".into()),
                    file: Some("/tmp/report.sql".into()),
                },
                SavedTab::Table {
                    schema: "public".into(),
                    table: "t".into(),
                    filter: "id > 1".into(),
                    order: Some(("id".into(), false)),
                    hidden: vec!["note".into()],
                    pinned: vec!["id".into()],
                },
            ],
            active: 1,
            window: Some([1200.0, 800.0]),
            split: Some(SavedSplit {
                right: vec![false, true],
                shown: [0, 1],
            }),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Saved>(&json).unwrap(), s);
        // Sessions saved before the split view have no `split`.
        let old: Saved =
            serde_json::from_str(r#"{"conn":"abc","tabs":[],"active":0,"window":null}"#).unwrap();
        assert_eq!(old.split, None);
        // A broken file just means "nothing to restore".
        assert!(serde_json::from_str::<Saved>("{oops").is_err());
    }
}
