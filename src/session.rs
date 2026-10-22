//! What was open when the app closed (connection, tabs, window size), restored on the next start.
//! Passwords are not part of it: reconnecting goes through the normal secret store.

use serde::{Deserialize, Serialize};

use crate::store::{self, Loaded, StoreError};

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct Saved {
    /// Id of the connection that was open.
    pub conn: Option<String>,
    /// The database that was in use (Postgres connects to one; MySQL switches with USE), and
    /// the Postgres schema first in the search path: a connection may have been moved off the
    /// ones it was saved with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub tabs: Vec<SavedTab>,
    pub active: usize,
    /// Inner window size in points.
    pub window: Option<[f32; 2]>,
    /// One entry per saved tab: pinned. Missing in older files (nothing pinned).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<bool>,
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
        /// Further sort columns, after `order`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        then_by: Vec<(String, bool)>,
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

/// The window a new start opens with when none was saved.
const DEFAULT_WINDOW: [f32; 2] = [1100.0, 700.0];

impl Saved {
    /// The saved window size, if it makes a usable window: one closed while minimized can be
    /// 0 x 0, and a hand-edited file can say anything (a too-big number reads as infinity).
    pub fn window_size(&self) -> [f32; 2] {
        match self.window {
            Some([w, h]) if (400.0..=16_384.0).contains(&w) && (300.0..=16_384.0).contains(&h) => [w, h],
            _ => DEFAULT_WINDOW,
        }
    }
}

pub fn load() -> Loaded<Saved> {
    store::load(FILE)
}

/// SQL editors can hold data, so the file is private like the others. Each window has its own
/// tabs: the last one closed is what the next start reopens, not a mix of them.
pub fn save(s: &Saved) -> Result<(), StoreError> {
    store::replace(FILE, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let s = Saved {
            conn: Some("abc".into()),
            database: Some("b".into()),
            schema: None,
            pinned: vec![true],
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
                    then_by: vec![("name".into(), true)],
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

    #[test]
    fn unusable_window_sizes_open_the_default() {
        let size = |window| {
            Saved {
                window,
                ..Default::default()
            }
            .window_size()
        };
        assert_eq!(size(Some([1200.0, 800.0])), [1200.0, 800.0]);
        for bad in [
            [0.0, 0.0],
            [-5.0, 700.0],
            [1100.0, f32::INFINITY],
            [f32::NAN, 700.0],
            [1e6, 700.0],
        ] {
            assert_eq!(size(Some(bad)), DEFAULT_WINDOW, "{bad:?}");
        }
        assert_eq!(size(None), DEFAULT_WINDOW);
        // What a file can hold that reads as infinity.
        let s: Saved =
            serde_json::from_str(r#"{"conn":null,"tabs":[],"active":0,"window":[1e39,700]}"#).unwrap();
        assert_eq!(s.window_size(), DEFAULT_WINDOW);
    }
}
