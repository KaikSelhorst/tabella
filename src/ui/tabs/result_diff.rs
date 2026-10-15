//! "Compare with the pinned one": the editor's current result against the pinned one, row by row.

use std::sync::Arc;

use super::*;
use crate::app::Pinned;
use crate::values::{ResultDiff, RowDiff};

const ROW_HEIGHT: f32 = 22.0;
const STATUS_WIDTH: f32 = 84.0;

/// Draws the comparison. Returns `true` when "Back to result" was clicked.
pub(super) fn diff_view(
    ui: &mut egui::Ui,
    tab_id: u64,
    pinned: &Pinned,
    res: &QueryResult,
    key: &mut Option<String>,
    hide_equal: &mut bool,
) -> bool {
    let t = theme::tokens();
    let a = &pinned.result;
    // Only columns both results have can pair rows up.
    let shared: Vec<&String> = res.columns.iter().filter(|c| a.columns.contains(c)).collect();
    if key.as_ref().is_some_and(|k| !shared.contains(&k)) {
        *key = None;
    }
    let d = cached_diff(ui.ctx(), tab_id, pinned, res, key.as_deref());
    let mut back = false;
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::GIT_DIFF).color(t.lime));
                let when = crate::history::ago(pinned.at, crate::history::now());
                ui.label(
                    egui::RichText::new(format!("Comparing with the result pinned {when}"))
                        .color(t.foreground),
                );
                let first = pinned
                    .sql
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect::<String>();
                ui.label(egui::RichText::new(first).monospace().size(11.0).color(t.subtle))
                    .on_hover_text(egui::RichText::new(&pinned.sql).monospace());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    back = ui.add(theme::outline_button("Back to result")).clicked();
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Match rows by")
                        .size(12.0)
                        .color(t.muted_foreground),
                );
                egui::ComboBox::from_id_salt(("diff_key", tab_id))
                    .selected_text(key.as_deref().unwrap_or("position"))
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(key, None, "position");
                        for c in &shared {
                            ui.selectable_value(key, Some((*c).clone()), c.as_str());
                        }
                    })
                    .response
                    .on_hover_text(
                        "The column that identifies the row in both results (e.g. the primary key)",
                    );
                ui.checkbox(hide_equal, "hide equal");
                ui.add_space(8.0);
                let chip = |ui: &mut egui::Ui, n: usize, text: &str, color: egui::Color32| {
                    let c = if n == 0 { t.subtle } else { color };
                    ui.label(egui::RichText::new(format!("{n} {text}")).size(12.0).color(c));
                };
                chip(ui, d.removed, "only in pinned", t.destructive);
                chip(ui, d.added, "only in current", t.lime);
                chip(ui, d.changed, "changed", t.warning);
                chip(ui, d.same, "equal", t.muted_foreground);
            });
        });
    let rows: Vec<&RowDiff> = d
        .rows
        .iter()
        .filter(|r| !(*hide_equal && matches!(r, RowDiff::Same(..))))
        .collect();
    if rows.is_empty() {
        ui.add_space(32.0);
        ui.vertical_centered(|ui| {
            let text = if d.rows.is_empty() {
                "Both results are empty"
            } else {
                "No differences"
            };
            ui.label(egui::RichText::new(text).color(t.subtle));
        });
        return back;
    }
    let widths = column_widths(&d, a, res);
    egui::ScrollArea::horizontal()
        .id_salt(("diff_h", tab_id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                ui.horizontal(|ui| {
                    cell(ui, STATUS_WIDTH, egui::RichText::new("").into());
                    for (c, name) in d.columns.iter().enumerate() {
                        let mut text = egui::RichText::new(name).strong().color(t.foreground);
                        if d.in_a[c].is_none() || d.in_b[c].is_none() {
                            // Only one side has it.
                            text = text.italics().color(t.muted_foreground);
                        }
                        cell(ui, widths[c], text.into()).on_hover_text(match (d.in_a[c], d.in_b[c]) {
                            (None, _) => "New column (only in current)",
                            (_, None) => "Column only in pinned",
                            _ => "",
                        });
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt(("diff_v", tab_id))
                    .auto_shrink([false, false])
                    .show_rows(ui, ROW_HEIGHT, rows.len(), |ui, range| {
                        for r in &rows[range] {
                            ui.horizontal(|ui| row_ui(ui, &d, a, res, r, &widths));
                        }
                    });
            });
        });
    back
}

/// The diff, computed again only when either result, the key or the pin changes.
fn cached_diff(
    ctx: &egui::Context,
    tab_id: u64,
    p: &Pinned,
    res: &QueryResult,
    key: Option<&str>,
) -> Arc<ResultDiff> {
    let slot = egui::Id::new(("result_diff", tab_id));
    let stamp = egui::Id::new((
        p.at,
        p.result.rows.len(),
        res.elapsed,
        res.rows.len(),
        &res.columns,
        key,
    ));
    if let Some((s, d)) = ctx.data(|m| m.get_temp::<(egui::Id, Arc<ResultDiff>)>(slot))
        && s == stamp
    {
        return d;
    }
    let d = Arc::new(values::diff_results(&p.result, res, key));
    ctx.data_mut(|m| m.insert_temp(slot, (stamp, d.clone())));
    d
}

/// Wide enough for the name and the first values (a changed cell shows both sides).
fn column_widths(d: &ResultDiff, a: &QueryResult, b: &QueryResult) -> Vec<f32> {
    let len = |v: Option<&Option<String>>| v.and_then(|v| v.as_deref()).map_or(4, |s| s.chars().count());
    d.columns
        .iter()
        .enumerate()
        .map(|(c, name)| {
            let widest = d
                .rows
                .iter()
                .take(60)
                .map(|r| {
                    let (x, y) = d.cell(a, b, r, c);
                    match r {
                        RowDiff::Changed(_, _, cols) if cols.contains(&c) => len(x) + len(y) + 3,
                        _ => len(x).max(len(y)),
                    }
                })
                .max()
                .unwrap_or(0)
                .max(name.chars().count());
            (widest as f32 * 7.2 + 16.0).clamp(64.0, 320.0)
        })
        .collect()
}

/// A fixed-width cell, text from its left edge (cut with "…" when longer).
fn cell(ui: &mut egui::Ui, width: f32, text: egui::WidgetText) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(width, ROW_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(egui::vec2(width, ROW_HEIGHT));
            ui.add_space(6.0);
            ui.add(egui::Label::new(text).truncate())
        },
    )
    .inner
}

fn row_ui(ui: &mut egui::Ui, d: &ResultDiff, a: &QueryResult, b: &QueryResult, r: &RowDiff, widths: &[f32]) {
    let t = theme::tokens();
    let (status, color) = match r {
        RowDiff::Removed(_) => ("removed", t.destructive),
        RowDiff::Added(_) => ("new", t.lime),
        RowDiff::Changed(..) => ("changed", t.warning),
        RowDiff::Same(..) => ("same", t.subtle),
    };
    cell(
        ui,
        STATUS_WIDTH,
        egui::RichText::new(status).size(12.0).color(color).into(),
    );
    let font = egui::FontId::monospace(12.0);
    let piece =
        |job: &mut egui::text::LayoutJob, v: Option<&Option<String>>, color: egui::Color32, strike: bool| {
            let (text, color, italics) = match v {
                None => ("—".to_string(), t.subtle, false),
                Some(None) => ("NULL".to_string(), t.subtle, true),
                Some(Some(s)) => (s.replace('\n', " "), color, false),
            };
            let mut fmt = egui::TextFormat::simple(font.clone(), color);
            fmt.italics = italics;
            if strike {
                fmt.strikethrough = egui::Stroke::new(1.0, color);
            }
            job.append(&text, 0.0, fmt);
        };
    for (c, w) in widths.iter().enumerate() {
        let (old, new) = d.cell(a, b, r, c);
        let mut job = egui::text::LayoutJob::default();
        match r {
            RowDiff::Changed(_, _, cols) if cols.contains(&c) => {
                piece(&mut job, old, t.destructive, true);
                job.append(" → ", 0.0, egui::TextFormat::simple(font.clone(), t.subtle));
                piece(&mut job, new, t.lime, false);
            }
            RowDiff::Removed(_) => piece(&mut job, old, t.destructive, false),
            RowDiff::Added(_) => piece(&mut job, new, t.lime, false),
            _ => piece(&mut job, new.or(old), t.muted_foreground, false),
        }
        let full = job.text.clone();
        let r = cell(ui, *w, job.into());
        if full.chars().count() > 30 {
            r.on_hover_text(full);
        }
    }
}
