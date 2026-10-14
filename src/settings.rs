//! Preferences (`settings.json`): theme, zoom, font sizes, paging, number format. The values
//! the rest of the app reads every frame live in atomics, so reading them costs nothing and
//! needs no plumbing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub light: bool,
    /// UI scale (Ctrl + / Ctrl − change it too).
    pub zoom: f32,
    pub editor_font: f32,
    pub grid_font: f32,
    /// Rows per page in table tabs.
    pub page_size: usize,
    /// Most rows kept from one editor query.
    pub max_rows: usize,
    /// Show numbers as 1234,5 instead of 1234.5 (display only; copying keeps the database's form).
    pub decimal_comma: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            light: false,
            zoom: 1.0,
            editor_font: 13.0,
            grid_font: 12.0,
            page_size: 200,
            max_rows: 5000,
            decimal_comma: false,
        }
    }
}

/// Allowed ranges, also used by the preferences dialog.
pub const ZOOM: std::ops::RangeInclusive<f32> = 0.75..=2.0;
pub const FONT: std::ops::RangeInclusive<f32> = 10.0..=20.0;
pub const PAGE: std::ops::RangeInclusive<usize> = 50..=2000;
pub const ROWS: std::ops::RangeInclusive<usize> = 500..=100_000;

impl Settings {
    /// Values from a hand-edited (or older) file pulled into range.
    pub fn clamped(mut self) -> Self {
        self.zoom = self.zoom.clamp(*ZOOM.start(), *ZOOM.end());
        self.editor_font = self.editor_font.clamp(*FONT.start(), *FONT.end());
        self.grid_font = self.grid_font.clamp(*FONT.start(), *FONT.end());
        self.page_size = self.page_size.clamp(*PAGE.start(), *PAGE.end());
        self.max_rows = self.max_rows.clamp(*ROWS.start(), *ROWS.end());
        self
    }
}

fn path() -> Option<PathBuf> {
    crate::config::dir().map(|d| d.join("settings.json"))
}

pub fn load() -> Settings {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .unwrap_or_default()
        .clamped()
}

pub fn save(s: &Settings) -> Result<(), String> {
    let path = path().ok_or("diretório de configuração não encontrado")?;
    let json = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    crate::config::write_private(&path, &json).map_err(|e| e.to_string())
}

static EDITOR_FONT: AtomicU32 = AtomicU32::new(0);
static GRID_FONT: AtomicU32 = AtomicU32::new(0);
static PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);
static MAX_ROWS: AtomicUsize = AtomicUsize::new(0);
static DECIMAL_COMMA: AtomicBool = AtomicBool::new(false);

/// Makes `s` the values everything reads (the theme is set separately: it needs the context).
pub fn publish(s: &Settings) {
    EDITOR_FONT.store(s.editor_font.to_bits(), Ordering::Relaxed);
    GRID_FONT.store(s.grid_font.to_bits(), Ordering::Relaxed);
    PAGE_SIZE.store(s.page_size, Ordering::Relaxed);
    MAX_ROWS.store(s.max_rows, Ordering::Relaxed);
    DECIMAL_COMMA.store(s.decimal_comma, Ordering::Relaxed);
}

fn float(a: &AtomicU32, default: f32) -> f32 {
    match a.load(Ordering::Relaxed) {
        0 => default,
        bits => f32::from_bits(bits),
    }
}

pub fn editor_font() -> f32 {
    float(&EDITOR_FONT, Settings::default().editor_font)
}

pub fn grid_font() -> f32 {
    float(&GRID_FONT, Settings::default().grid_font)
}

pub fn page_size() -> usize {
    match PAGE_SIZE.load(Ordering::Relaxed) {
        0 => Settings::default().page_size,
        n => n,
    }
}

pub fn max_rows() -> usize {
    match MAX_ROWS.load(Ordering::Relaxed) {
        0 => Settings::default().max_rows,
        n => n,
    }
}

pub fn decimal_comma() -> bool {
    DECIMAL_COMMA.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_wild_values_fall_back() {
        let s: Settings = serde_json::from_str(r#"{"light": true, "page_size": 3}"#).unwrap();
        let s = s.clamped();
        assert!(s.light);
        assert_eq!(s.page_size, 50, "pulled into range");
        assert_eq!(s.editor_font, 13.0, "missing keys keep the default");
    }
}
