//! Preferences kept between runs (`settings.json`): the theme and the zoom.

use serde::{Deserialize, Serialize};

use crate::store::{self, Loaded, StoreError};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub light: bool,
    /// UI scale (Ctrl + / Ctrl − / Ctrl 0, or Ctrl+P).
    pub zoom: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            light: false,
            zoom: 1.0,
        }
    }
}

/// Zoom limits and the step of "Zoom in / Zoom out".
pub const ZOOM: std::ops::RangeInclusive<f32> = 0.5..=3.0;
pub const ZOOM_STEP: f32 = 0.1;

impl Settings {
    /// Values from a hand-edited (or older) file pulled into range.
    pub fn clamped(mut self) -> Self {
        self.zoom = self.zoom.clamp(*ZOOM.start(), *ZOOM.end());
        self
    }
}

const FILE: &str = "settings.json";

pub fn load() -> Loaded<Settings> {
    let mut l: Loaded<Settings> = store::load(FILE);
    l.value = l.value.clamped();
    l
}

pub fn save(s: &Settings) -> Result<(), StoreError> {
    store::save(FILE, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_wild_values_fall_back() {
        // Older files may still carry options that no longer exist: they are ignored.
        let s: Settings = serde_json::from_str(r#"{"light": true, "zoom": 9, "page_size": 3}"#).unwrap();
        let s = s.clamped();
        assert!(s.light);
        assert_eq!(s.zoom, 3.0, "pulled into range");
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
    }
}
