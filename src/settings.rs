//! Preferences kept between runs (`settings.json`): the theme and the zoom.

use serde::{Deserialize, Serialize};

use crate::store::{self, Loaded, StoreError};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub light: bool,
    /// Match the operating system's light/dark setting (`light` then holds the last one seen, so
    /// the next start opens in it). A file from before this option keeps the theme it had.
    #[serde(default)]
    pub system_theme: bool,
    /// UI scale (Ctrl + / Ctrl − / Ctrl 0, or Ctrl+P).
    pub zoom: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            light: false,
            // A first run follows the system.
            system_theme: true,
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
        // A settings file from before "follow the system" keeps its theme.
        assert!(!s.system_theme);
        // No file at all (a first run) follows the system; a file that lacks the option doesn't.
        assert!(Settings::default().system_theme);
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            s,
            Settings {
                system_theme: false,
                ..Settings::default()
            }
        );
    }
}
