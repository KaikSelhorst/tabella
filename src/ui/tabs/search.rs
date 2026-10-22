//! "Search value in all tables".

use super::*;
use std::fmt::Write as _;

/// "Search value in all tables": the term, options, progress and where it was found.
pub(super) fn search_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let t = theme::tokens();
    let TabKind::Search(st) = &mut tab.kind else {
        return;
    };
    let (mut start, mut cancel) = (false, false);
    let mut open: Option<(String, String, String)> = None;
    egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::MAGNIFYING_GLASS).size(15.0).color(t.muted_foreground));
                ui.label(egui::RichText::new("Search value in all tables").size(15.0).color(t.foreground));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().interact_size.y = 28.0;
                let r = ui.add_enabled(
                    !st.running,
                    TextEdit::singleline(&mut st.term)
                        .hint_text("e-mail, SSN, code, id…  (Enter searches)")
                        .font(egui::FontId::monospace(13.0))
                        .margin(egui::Margin::symmetric(8, 6))
                        .desired_width(360.0),
                );
                if std::mem::take(&mut st.focus) {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    start = true;
                }
                if st.running {
                    if ui.add(theme::outline_button("Cancel")).clicked() {
                        cancel = true;
                    }
                } else if ui
                    .add_enabled(!st.term.trim().is_empty(), theme::primary_button(Some(icon::MAGNIFYING_GLASS), "Search"))
                    .clicked()
                {
                    start = true;
                }
                ui.add_enabled(!st.running, egui::Checkbox::new(&mut st.exact, "exact value"))
                    .on_hover_text("Unchecked: looks for the text inside values (any case)");
                ui.add_enabled(
                    !st.running,
                    egui::Checkbox::new(&mut st.include_big, "include large tables"),
                )
                .on_hover_text("Tables with over 1 million rows: each one is read in full and takes seconds");
            });
            ui.label(
                egui::RichText::new(
                    "Reads the tables one by one (only columns where the value would fit). Meanwhile, the other tabs \
                     of this connection wait.",
                )
                .size(12.0)
                .color(t.subtle),
            );
            if st.running || st.total > 0 {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let frac = if st.total == 0 { 0.0 } else { st.done as f32 / st.total as f32 };
                    ui.add(egui::ProgressBar::new(frac).desired_width(240.0).desired_height(6.0).fill(t.accent_text));
                    let mut text = format!(
                        "{} of {} tables · found in {}",
                        st.done,
                        st.total,
                        st.hits.len()
                    );
                    if st.skipped > 0 {
                        let _ = write!(text, " · {} large ones skipped", st.skipped);
                    }
                    if !st.errors.is_empty() {
                        let _ = write!(text, " · {} with errors", st.errors.len());
                    }
                    let r = ui.label(egui::RichText::new(text).size(12.0).color(t.muted_foreground));
                    if !st.errors.is_empty() {
                        r.on_hover_text(st.errors.join("\n"));
                    }
                });
            }
        });

    egui::ScrollArea::vertical()
        .id_salt(("search_hits", tab.id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 4))
                .show(ui, |ui| {
                    if st.hits.is_empty() {
                        if !st.running && st.total > 0 {
                            ui.label(egui::RichText::new("Nothing found").color(t.subtle));
                        }
                        return;
                    }
                    egui::Grid::new(("search_grid", tab.id))
                        .num_columns(4)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            for h in ["table", "columns", "rows", ""] {
                                ui.label(egui::RichText::new(h).size(12.0).color(t.subtle));
                            }
                            ui.end_row();
                            for h in &st.hits {
                                ui.label(
                                    egui::RichText::new(format!("{}.{}", h.schema, h.table))
                                        .monospace()
                                        .color(t.foreground),
                                );
                                ui.label(
                                    egui::RichText::new(h.columns.join(", "))
                                        .monospace()
                                        .color(t.info),
                                );
                                let rows = if h.rows >= crate::search::ROWS_PER_TABLE {
                                    format!("{}+", h.rows)
                                } else {
                                    h.rows.to_string()
                                };
                                ui.label(egui::RichText::new(rows).monospace().color(t.number));
                                if ui
                                    .button(theme::icon_text(
                                        icon::TABLE,
                                        "Open",
                                        t.muted_foreground,
                                        t.foreground,
                                    ))
                                    .on_hover_text(egui::RichText::new(&h.filter).monospace())
                                    .clicked()
                                {
                                    open = Some((h.schema.clone(), h.table.clone(), h.filter.clone()));
                                }
                                ui.end_row();
                            }
                        });
                });
        });
    if start {
        app::start_search(tab, s);
    } else if cancel {
        app::cancel_search(tab, s);
    }
    if let Some((schema, table, filter)) = open {
        cx.request = Some(Request::OpenFiltered(schema, table, filter));
    }
}
