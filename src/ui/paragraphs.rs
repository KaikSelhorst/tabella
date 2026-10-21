//! A long text laid out a paragraph at a time, each paragraph's layout kept from frame to frame.
//! egui caches paragraphs too, but drops whatever wasn't laid out in the last frame: an editor
//! that keeps its whole galley while the text stays the same lost them all, and its first
//! keystroke after a pause laid the whole text out again (1.7 s for a 700 KB paste).

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::{
    self, Galley,
    text::{ByteIndex, LayoutJob, LayoutSection},
};

/// Paragraph layouts of the last text laid out under an id, by a hash of their job.
type Kept = HashMap<u64, Arc<Galley>>;

/// `fonts.layout_job(job)`, reusing what `cache` kept of the previous text; it then keeps only
/// this text's paragraphs. Remove `cache` from the context's data along with its owner.
pub fn layout(ui: &egui::Ui, cache: egui::Id, mut job: LayoutJob) -> Arc<Galley> {
    // What egui splits (elided text is short anyway).
    if !(job.break_on_newline && job.wrap.max_rows == usize::MAX && job.text.contains('\n')) {
        return ui.fonts_mut(|f| f.layout_job(job));
    }
    // As egui does before laying out, so the paragraphs' width is what their layout got.
    if job.wrap.max_width.is_finite() {
        job.wrap.max_width = job.wrap.max_width.round();
    }
    let ppp = ui.ctx().pixels_per_point();
    let mut kept: Kept = ui.data_mut(|d| d.remove_temp(cache)).unwrap_or_default();
    let mut now = Kept::with_capacity(kept.len());
    let mut galleys = Vec::new();
    let mut missing = Vec::new();
    for p in split(&job) {
        let hash = egui::epaint::util::hash((&p, ppp.to_bits()));
        match kept.remove(&hash).or_else(|| now.get(&hash).cloned()) {
            Some(g) => {
                now.insert(hash, Arc::clone(&g));
                galleys.push(Some(g));
            }
            None => {
                missing.push((galleys.len(), hash, p));
                galleys.push(None);
            }
        }
    }
    if !missing.is_empty() {
        ui.fonts_mut(|f| {
            for (i, hash, p) in missing {
                let g = f.layout_job(p);
                now.insert(hash, Arc::clone(&g));
                galleys[i] = Some(g);
            }
        });
    }
    ui.data_mut(|d| d.insert_temp(cache, now));
    let galleys: Vec<Arc<Galley>> = galleys.into_iter().flatten().collect();
    Arc::new(Galley::concat(Arc::new(job), &galleys, ppp))
}

/// The job's paragraphs, cut as egui cuts them (`layout_each_paragraph_individually`): at each
/// `\n`, which belongs to neither side, except a final one, kept in the last paragraph.
fn split(job: &LayoutJob) -> Vec<LayoutJob> {
    let mut out = Vec::new();
    let mut section = 0;
    let mut start = 0;
    while start < job.text.len() {
        let mut end = job.text[start..].find('\n').map_or(job.text.len(), |i| start + i);
        if end == job.text.len() - 1 && job.text.ends_with('\n') {
            end += 1;
        }
        let mut p = LayoutJob {
            text: job.text[start..end].to_owned(),
            wrap: job.wrap.clone(),
            sections: Vec::new(),
            break_on_newline: job.break_on_newline,
            halign: job.halign,
            justify: job.justify,
            first_row_min_height: if start == 0 { job.first_row_min_height } else { 0.0 },
            round_output_to_gui: job.round_output_to_gui,
            keep_trailing_whitespace: job.keep_trailing_whitespace,
        };
        for s in &job.sections[section..] {
            if s.byte_range.end <= ByteIndex(start) {
                section += 1;
            } else if ByteIndex(end) < s.byte_range.start {
                break;
            } else {
                p.sections.push(LayoutSection {
                    leading_space: if ByteIndex(start) <= s.byte_range.start {
                        s.leading_space
                    } else {
                        0.0
                    },
                    byte_range: s.byte_range.start.saturating_sub(start)
                        ..s.byte_range.end.min(ByteIndex(end)).saturating_sub(start),
                    format: s.format.clone(),
                });
            }
        }
        out.push(p);
        start = end + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same galley egui lays out in one go, and a changed line is the only one laid out
    /// again.
    #[test]
    fn matches_egui_and_keeps_paragraphs() {
        let ctx = egui::Context::default();
        // No renderer here: the font textures are let go.
        let frame =
            |f: &mut dyn FnMut(&mut egui::Ui)| ctx.run_ui(Default::default(), f).textures_delta.clear();
        let job = |text: &str| {
            let mut j = crate::ui::syntax::highlight(text, &egui::FontId::monospace(13.0));
            j.wrap.max_width = 300.4;
            j
        };
        let text = "SELECT a, 'x'\n/* two\nlines */ FROM t\n\nWHERE id = :id;\n";
        let cache = egui::Id::new("paragraphs_test");
        let mut ours = None;
        let mut theirs = None;
        frame(&mut |ui| {
            ours = Some(layout(ui, cache, job(text)));
            theirs = Some(ui.fonts_mut(|f| f.layout_job(job(text))));
        });
        let (ours, theirs) = (ours.unwrap(), theirs.unwrap());
        assert_eq!(ours.text(), theirs.text());
        assert_eq!(ours.rect, theirs.rect);
        let rows = |g: &Galley| {
            g.rows
                .iter()
                .map(|r| (r.pos, r.ends_with_newline, r.char_count_excluding_newline()))
                .collect::<Vec<_>>()
        };
        assert_eq!(rows(&ours), rows(&theirs));

        // Frames later, one line changed: the others come from what was kept.
        for _ in 0..3 {
            frame(&mut |_| {});
        }
        let changed = text.replace("FROM t", "FROM u");
        frame(&mut |ui| {
            let before: Kept = ui.data(|d| d.get_temp(cache)).unwrap();
            let g = layout(ui, cache, job(&changed));
            let after: Kept = ui.data(|d| d.get_temp(cache)).unwrap();
            assert_eq!(g.text(), changed);
            let reused = after.keys().filter(|k| before.contains_key(k)).count();
            assert_eq!((after.len(), reused), (5, 4), "only the changed line is new");
        });
    }

    #[test]
    fn sections_follow_their_paragraph() {
        let mut j = crate::ui::syntax::highlight("ab\n/* c\nd */\n", &egui::FontId::monospace(13.0));
        j.wrap.max_width = f32::INFINITY;
        let parts = split(&j);
        assert_eq!(
            parts.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(),
            ["ab", "/* c", "d */\n"]
        );
        for p in &parts {
            assert_eq!(p.sections.first().unwrap().byte_range.start, ByteIndex(0));
            assert_eq!(p.sections.last().unwrap().byte_range.end, ByteIndex(p.text.len()));
        }
    }
}
