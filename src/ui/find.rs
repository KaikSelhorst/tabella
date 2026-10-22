//! Find and replace in the SQL editor: matching, highlighting in the layout, and the bar.

use crate::ui::theme::A11y;
use std::ops::Range;

use eframe::egui::{self, Key, RichText, TextEdit, epaint::text::ByteIndex, text::LayoutJob};

use super::{icon, theme};

#[derive(Clone, Default)]
pub struct FindState {
    pub query: String,
    pub replace: String,
    pub show_replace: bool,
    pub case_sensitive: bool,
    /// Match the editor should select and scroll to.
    pub current: usize,
    /// Focus the search field on the next frame (just opened with Ctrl+F).
    pub focus: bool,
}

/// What the bar asks the editor to do this frame.
#[derive(Debug, PartialEq)]
pub enum FindAction {
    /// Select match `current` and scroll to it.
    Goto,
    ReplaceOne,
    ReplaceAll,
    Close,
}

/// Byte ranges of `needle` in `hay`, left to right, not overlapping.
pub fn find_all(hay: &str, needle: &str, case_sensitive: bool) -> Vec<Range<usize>> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = hay.char_indices().collect();
    let same = |a: char, b: char| a == b || (!case_sensitive && a.to_lowercase().eq(b.to_lowercase()));
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= chars.len() {
        if (0..needle.len()).all(|k| same(chars[i + k].1, needle[k])) {
            let end = chars.get(i + needle.len()).map_or(hay.len(), |(b, _)| *b);
            out.push(chars[i].0..end);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Gives the matched text a background in an already highlighted layout.
pub fn mark(job: &mut LayoutJob, ranges: &[Range<usize>], current: usize) {
    if ranges.is_empty() {
        return;
    }
    let t = theme::tokens();
    let (bg, bg_current) = (
        theme::mix(t.warning, 0.30, t.background),
        theme::mix(t.lime, 0.55, t.background),
    );
    let mut out = Vec::with_capacity(job.sections.len() + ranges.len() * 2);
    for sec in job.sections.drain(..) {
        let (mut start, end) = (sec.byte_range.start.0, sec.byte_range.end.0);
        let first = ranges.partition_point(|r| r.end <= start);
        let mut cuts: Vec<usize> = ranges[first..]
            .iter()
            .take_while(|r| r.start < end)
            .flat_map(|r| [r.start, r.end])
            .filter(|&p| p > start && p < end)
            .collect();
        cuts.push(end);
        for (n, cut) in cuts.into_iter().enumerate() {
            let mut piece = sec.clone();
            piece.byte_range = ByteIndex(start)..ByteIndex(cut);
            if n > 0 {
                piece.leading_space = 0.0;
            }
            if let Some(i) = ranges[first..]
                .iter()
                .position(|r| r.start <= start && start < r.end)
                .map(|i| i + first)
            {
                piece.format.background = if i == current { bg_current } else { bg };
            }
            out.push(piece);
            start = cut;
        }
    }
    job.sections = out;
}

/// Replaces every match; returns how many.
pub fn replace_all(text: &mut String, needle: &str, with: &str, case_sensitive: bool) -> usize {
    let ranges = find_all(text, needle, case_sensitive);
    for r in ranges.iter().rev() {
        text.replace_range(r.clone(), with);
    }
    ranges.len()
}

/// The bar above the editor. `matches` is how many the current query finds.
pub fn find_bar(ui: &mut egui::Ui, st: &mut FindState, matches: usize) -> Option<FindAction> {
    let t = theme::tokens();
    let mut action = None;
    egui::Frame::new()
        .fill(t.card)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let r = ui.add(
                    TextEdit::singleline(&mut st.query)
                        .hint_text("Find")
                        .font(egui::FontId::monospace(12.5))
                        .desired_width(220.0),
                );
                if std::mem::take(&mut st.focus) {
                    r.request_focus();
                }
                if r.changed() {
                    st.current = 0;
                    action = Some(FindAction::Goto);
                }
                // Enter: next; Shift+Enter: previous. The field keeps focus to go on typing.
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    let back = ui.input(|i| i.modifiers.shift);
                    step(st, matches, back);
                    action = Some(FindAction::Goto);
                    r.request_focus();
                }
                let count = match matches {
                    0 if st.query.is_empty() => String::new(),
                    0 => "none".to_string(),
                    n => format!("{} of {n}", st.current.min(n - 1) + 1),
                };
                let color = if matches == 0 && !st.query.is_empty() {
                    t.destructive
                } else {
                    t.subtle
                };
                ui.label(RichText::new(count).size(12.0).color(color));
                let arrow = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
                    ui.add_enabled(
                        matches > 0,
                        egui::Button::new(theme::icon(glyph).color(t.muted_foreground)).frame(false),
                    )
                    .tip(tip)
                    .clicked()
                };
                if arrow(ui, icon::ARROW_UP, "Previous (Shift+Enter)") {
                    step(st, matches, true);
                    action = Some(FindAction::Goto);
                }
                if arrow(ui, icon::ARROW_DOWN, "Next (Enter)") {
                    step(st, matches, false);
                    action = Some(FindAction::Goto);
                }
                let case_color = if st.case_sensitive {
                    t.accent_text
                } else {
                    t.muted_foreground
                };
                if ui
                    .add(egui::Button::new(RichText::new("Aa").size(12.0).color(case_color)).frame(false))
                    .on_hover_text("Match case")
                    .clicked()
                {
                    st.case_sensitive = !st.case_sensitive;
                    st.current = 0;
                }
                if st.show_replace {
                    ui.add_space(8.0);
                    ui.add(
                        TextEdit::singleline(&mut st.replace)
                            .hint_text("Replace with")
                            .font(egui::FontId::monospace(12.5))
                            .desired_width(180.0),
                    );
                    if ui
                        .add_enabled(matches > 0, egui::Button::new("Replace"))
                        .clicked()
                    {
                        action = Some(FindAction::ReplaceOne);
                    }
                    if ui.add_enabled(matches > 0, egui::Button::new("All")).clicked() {
                        action = Some(FindAction::ReplaceAll);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(theme::icon(icon::X).color(t.muted_foreground)).frame(false))
                        .tip("Close (Esc)")
                        .clicked()
                    {
                        action = Some(FindAction::Close);
                    }
                });
            });
        });
    if ui.input(|i| i.key_pressed(Key::Escape)) {
        action = Some(FindAction::Close);
    }
    action
}

fn step(st: &mut FindState, matches: usize, back: bool) {
    if matches == 0 {
        return;
    }
    let cur = st.current.min(matches - 1);
    st.current = if back {
        (cur + matches - 1) % matches
    } else {
        (cur + 1) % matches
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_replaces() {
        let text = "SELECT Name, name FROM t WHERE café = 'NAME'";
        let r = find_all(text, "name", false);
        assert_eq!(r.len(), 3);
        assert_eq!(&text[r[2].clone()], "NAME");
        assert_eq!(find_all(text, "name", true).len(), 1);
        // Multi-byte chars keep byte ranges valid.
        let r = find_all(text, "CAFÉ", false);
        assert_eq!(&text[r[0].clone()], "café");
        let mut s = text.to_string();
        assert_eq!(replace_all(&mut s, "name", "x", false), 3);
        assert_eq!(s, "SELECT x, x FROM t WHERE café = 'x'");
        assert!(find_all("aaa", "", false).is_empty());
        assert_eq!(find_all("aaaa", "aa", false).len(), 2, "no overlaps");
    }

    #[test]
    fn marks_matches_across_sections() {
        let mut job = LayoutJob::default();
        let f = egui::TextFormat::default();
        job.append("SELECT ", 0.0, f.clone());
        job.append("name", 0.0, f.clone());
        job.append(" FROM t", 0.0, f);
        // "T na" spans two sections; "t" at the end is another match.
        let ranges = find_all(&job.text, "t na", false);
        assert_eq!(ranges, vec![5..9]);
        mark(&mut job, &ranges, 0);
        let marked: String = job
            .sections
            .iter()
            .filter(|s| s.format.background != egui::Color32::TRANSPARENT)
            .map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0])
            .collect();
        assert_eq!(marked, "T na");
        let total: usize = job
            .sections
            .iter()
            .map(|s| s.byte_range.end.0 - s.byte_range.start.0)
            .sum();
        assert_eq!(total, job.text.len());
    }

    /// Ctrl+F on any text with any needle: matches are slices of the text, in order, apart, and
    /// equal to the needle (ignoring case when asked); replacing does as many.
    #[test]
    fn any_needle_in_any_text() {
        use crate::testgen::{each_text, on_boundary};
        each_text(12, 3000, |hay, g| {
            let needle = g.text_of(2);
            for case in [true, false] {
                let found = find_all(hay, &needle, case);
                let mut end = 0;
                for r in &found {
                    assert!(end <= r.start && r.start < r.end, "{found:?}");
                    assert!(on_boundary(hay, r.start) && on_boundary(hay, r.end), "{r:?}");
                    if case {
                        assert_eq!(&hay[r.clone()], needle);
                    }
                    end = r.end;
                }
                let mut text = hay.to_string();
                assert_eq!(replace_all(&mut text, &needle, "#", case), found.len());
            }
        });
    }
}
