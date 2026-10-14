//! Right-hand panel showing the selected row as a vertical form: easier to read and edit
//! than a wide grid.

use eframe::egui::{self, FontId, RichText, TextEdit};

use super::grid::display_value;
use super::{icon, theme};
use crate::app::CopyAs;
use crate::db::{ColumnInfo, QueryResult};
use crate::values::{ValueKind, column_info};

pub enum DetailAction {
    Commit(usize, Option<String>),
    Copy(CopyAs),
    Close,
}

/// Values longer than this get a multi-line editor.
const LONG_VALUE: usize = 48;

/// The row shown in the panel and what can be done with it.
pub struct DetailRow<'a> {
    pub tab_id: u64,
    pub res: &'a QueryResult,
    pub cols: &'a [ColumnInfo],
    pub kinds: &'a [ValueKind],
    pub row: usize,
    /// 1-based position in the whole table (page offset included), for the title.
    pub row_number: usize,
    pub can_edit: bool,
}

pub fn detail_panel(ui: &mut egui::Ui, d: &DetailRow) -> Option<DetailAction> {
    let DetailRow {
        tab_id,
        res,
        cols,
        kinds,
        row,
        row_number,
        can_edit,
    } = *d;
    let t = theme::tokens();
    let mut action = None;
    let info = column_info(res, cols);

    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Linha {row_number}")).color(t.foreground));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let button = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
                ui.add(egui::Button::new(theme::icon(glyph).color(t.muted_foreground)).frame(false))
                    .on_hover_text(tip)
                    .clicked()
            };
            if button(ui, icon::X, "Fechar painel") {
                action = Some(DetailAction::Close);
            }
            if button(ui, icon::CODE, "Copiar como INSERT") {
                action = Some(DetailAction::Copy(CopyAs::Insert));
            }
            if button(ui, icon::BRACKETS_CURLY, "Copiar como JSON") {
                action = Some(DetailAction::Copy(CopyAs::Json));
            }
        });
    });
    ui.add_space(4.0);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (ci, name) in res.columns.iter().enumerate() {
                let value = &res.rows[row][ci];
                let c = info[ci];
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if c.is_some_and(|c| c.pk) {
                        ui.label(theme::icon(icon::KEY).size(11.0).color(t.warning));
                    }
                    ui.label(RichText::new(name).size(11.0).color(t.muted_foreground));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Booleans have NULL inside their own selector below.
                        let nullable = c.is_none_or(|c| c.nullable);
                        if can_edit && nullable && value.is_some() && kinds[ci] != ValueKind::Bool {
                            let null = egui::Button::new(RichText::new("NULL").size(10.0).color(t.subtle))
                                .frame(false);
                            if ui.add(null).on_hover_text("Definir NULL").clicked() {
                                action = Some(DetailAction::Commit(ci, None));
                            }
                        }
                        if let Some(c) = c {
                            ui.label(RichText::new(&c.data_type).size(10.0).color(t.subtle));
                        }
                    });
                });
                if can_edit && kinds[ci] == ValueKind::Bool {
                    // Segmented true | false | NULL (NULL only when the column allows it).
                    let now = value.as_deref().and_then(crate::sql::parse_bool);
                    let nullable = c.is_none_or(|c| c.nullable);
                    let mut options: Vec<(Option<bool>, &str)> =
                        vec![(Some(true), "true"), (Some(false), "false")];
                    if nullable {
                        options.push((None, "NULL"));
                    }
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for (v, label) in options {
                            let on = now == v && (v.is_some() || value.is_none());
                            let color = match (on, v) {
                                (false, _) => t.subtle,
                                (true, Some(true)) => t.boolean,
                                (true, Some(false)) => t.foreground,
                                (true, None) => t.muted_foreground,
                            };
                            let mut text = RichText::new(label).monospace().size(12.0).color(color);
                            if v.is_none() {
                                text = text.italics();
                            }
                            let button = egui::Button::new(text)
                                .selected(on)
                                .min_size(egui::vec2(64.0, 24.0));
                            if ui.add(button).clicked() && !on {
                                action = Some(DetailAction::Commit(ci, v.map(|b| b.to_string())));
                            }
                        }
                    });
                } else if can_edit {
                    // The buffer is keyed by the current value, so it resets after the row is refreshed.
                    let id = egui::Id::new(("detail", tab_id, row, ci, value));
                    let original = value.clone().unwrap_or_default();
                    let mut text = ui
                        .data_mut(|d| d.get_temp::<String>(id))
                        .unwrap_or_else(|| original.clone());
                    let long = original.len() > LONG_VALUE || original.contains('\n');
                    let edit = if long {
                        TextEdit::multiline(&mut text).desired_rows(3)
                    } else {
                        TextEdit::singleline(&mut text)
                    };
                    let r = ui.add(
                        edit.font(FontId::monospace(12.0))
                            .hint_text(if value.is_none() { "NULL" } else { "" })
                            .desired_width(f32::INFINITY),
                    );
                    if r.lost_focus() && text != original {
                        action = Some(DetailAction::Commit(ci, Some(text.clone())));
                    }
                    ui.data_mut(|d| d.insert_temp(id, text));
                } else {
                    let shown = match value {
                        Some(v) => RichText::new(display_value(kinds[ci], v))
                            .monospace()
                            .size(12.0)
                            .color(if kinds[ci] == ValueKind::Number {
                                t.number
                            } else {
                                t.foreground
                            }),
                        None => RichText::new("NULL")
                            .monospace()
                            .size(12.0)
                            .italics()
                            .color(t.subtle),
                    };
                    egui::Frame::new()
                        .fill(t.sidebar)
                        .inner_margin(egui::Margin::symmetric(6, 3))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(egui::Label::new(shown).wrap().selectable(true));
                        });
                }
            }
            ui.add_space(8.0);
            if can_edit {
                ui.label(
                    RichText::new("Edite e saia do campo para salvar")
                        .size(11.0)
                        .color(t.subtle),
                );
            }
        });
    action
}
