//! Right-hand panel showing the selected row as a vertical form: easier to read and edit
//! than a wide grid.

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::{self, FontId, RichText, TextEdit};
use serde_json::Value;

use super::grid::display_value;
use super::{icon, theme};
use crate::app::CopyAs;
use crate::db::{ColumnInfo, QueryResult};
use crate::values::{ValueKind, column_info};

pub enum DetailAction {
    Commit(usize, Option<String>),
    /// Choose the foreign key's value from the referenced rows.
    PickFk(usize),
    /// The field's value in the big editor.
    Open(usize),
    Copy(CopyAs),
    Close,
}

/// Space kept free on the right of the fields, where the (floating) scroll bar draws.
const SCROLL_BAR_ROOM: f32 = 10.0;

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

/// Children shown per object/array before "… N more".
const JSON_CHILDREN: usize = 200;

/// What the panel keeps between frames, per column: one entry each, replaced when the value
/// changes, so browsing many rows doesn't pile up. One map per tab, dropped with it
/// (`forget_tab_caches`).
#[derive(Clone, Default)]
pub(crate) struct DetailCache {
    /// The value parsed as JSON (objects and arrays), by the value it came from.
    json: HashMap<usize, (egui::Id, Option<Arc<Value>>)>,
    /// What is typed in an editable field, by the row and value it started from.
    text: HashMap<usize, (egui::Id, String)>,
}

pub(crate) fn cache_id(tab_id: u64) -> egui::Id {
    egui::Id::new(("detail_cache", tab_id))
}

/// The value parsed as JSON when it is an object or array; cached, since this runs every frame.
fn parsed_json(ctx: &egui::Context, tab_id: u64, ci: usize, v: &str) -> Option<Arc<Value>> {
    let s = v.trim_start();
    if !(s.starts_with('{') || s.starts_with('[')) {
        return None;
    }
    let hash = egui::Id::new(v);
    let cache = cache_id(tab_id);
    if let Some(cached) = ctx.data_mut(|d| {
        let c = d.get_temp_mut_or_default::<DetailCache>(cache);
        c.json
            .get(&ci)
            .filter(|(h, _)| *h == hash)
            .map(|(_, j)| j.clone())
    }) {
        return cached;
    }
    let parsed = serde_json::from_str::<Value>(v).ok().map(Arc::new);
    ctx.data_mut(|d| {
        let c = d.get_temp_mut_or_default::<DetailCache>(cache);
        c.json.insert(ci, (hash, parsed.clone()));
    });
    parsed
}

/// Collapsible tree: objects and arrays fold, leaves are colored by type. Right-click copies a
/// value or its path (`$.items[2].price`).
fn json_tree(ui: &mut egui::Ui, id: egui::Id, key: Option<&str>, path: &str, v: &Value, depth: usize) {
    let t = theme::tokens();
    let key_text = |k: &str| RichText::new(k).monospace().size(12.0).color(t.muted_foreground);
    let Some(Container {
        open,
        close,
        len: n,
        children,
    }) = container(v, path)
    else {
        let (text, color) = match v {
            Value::String(s) => (serde_json::to_string(s).unwrap_or_default(), t.foreground),
            Value::Number(n) => (n.to_string(), t.number),
            Value::Bool(b) => (b.to_string(), t.boolean),
            _ => ("null".to_string(), t.subtle),
        };
        let r = ui
            .horizontal_wrapped(|ui| {
                // Lines up with folded siblings, whose arrow takes this space.
                ui.add_space(ui.spacing().indent);
                if let Some(k) = key {
                    ui.label(key_text(&format!("{k}:")));
                }
                ui.add(egui::Label::new(RichText::new(text).monospace().size(12.0).color(color)).wrap())
            })
            .inner;
        r.context_menu(|ui| copy_menu(ui, path, v));
        return;
    };
    let summary = format!("{open} {n} {close}");
    let header = match key {
        Some(k) => format!("{k}  {summary}"),
        None => summary,
    };
    let r = egui::CollapsingHeader::new(RichText::new(header).monospace().size(12.0).color(t.foreground))
        .id_salt(id)
        .default_open(depth < 2)
        .show(ui, |ui| {
            for (shown, (k, p, c)) in children.enumerate() {
                if shown == JSON_CHILDREN {
                    ui.label(
                        RichText::new(format!("… {} more", n - shown))
                            .size(11.0)
                            .color(t.subtle),
                    );
                    break;
                }
                json_tree(ui, id.with(&k), Some(&k), &p, c, depth + 1);
            }
        });
    r.header_response.context_menu(|ui| copy_menu(ui, path, v));
}

/// An object or array as the tree shows it.
struct Container<'a> {
    open: &'static str,
    close: &'static str,
    len: usize,
    /// (label, path, value) of each child.
    children: Box<dyn Iterator<Item = (String, String, &'a Value)> + 'a>,
}

/// `None` for a leaf (string, number, bool, null).
fn container<'a>(v: &'a Value, path: &'a str) -> Option<Container<'a>> {
    match v {
        Value::Object(m) => Some(Container {
            open: "{",
            close: "}",
            len: m.len(),
            children: Box::new(m.iter().map(move |(k, c)| {
                let p = if k.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
                    format!("{path}.{k}")
                } else {
                    format!("{path}[{}]", serde_json::to_string(k).unwrap_or_default())
                };
                (k.clone(), p, c)
            })),
        }),
        Value::Array(a) => Some(Container {
            open: "[",
            close: "]",
            len: a.len(),
            children: Box::new(
                a.iter()
                    .enumerate()
                    .map(move |(i, c)| (i.to_string(), format!("{path}[{i}]"), c)),
            ),
        }),
        _ => None,
    }
}

fn copy_menu(ui: &mut egui::Ui, path: &str, v: &Value) {
    if ui.button("Copy value").clicked() {
        let text = match v {
            Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        ui.ctx().copy_text(text);
        ui.close();
    }
    if ui.button(format!("Copy path  {path}")).clicked() {
        ui.ctx().copy_text(path.to_string());
        ui.close();
    }
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
        ui.label(RichText::new(format!("Row {row_number}")).color(t.foreground));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let button = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
                ui.add(egui::Button::new(theme::icon(glyph).color(t.muted_foreground)).frame(false))
                    .on_hover_text(tip)
                    .clicked()
            };
            if button(ui, icon::X, "Close panel") {
                action = Some(DetailAction::Close);
            }
            if button(ui, icon::CODE, "Copy as INSERT") {
                action = Some(DetailAction::Copy(CopyAs::Insert));
            }
            if button(ui, icon::BRACKETS_CURLY, "Copy as JSON") {
                action = Some(DetailAction::Copy(CopyAs::Json));
            }
        });
    });
    ui.add_space(4.0);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // The scroll bar floats over the content: keep the fields clear of it.
            ui.add_space(0.0);
            ui.set_width(ui.available_width() - SCROLL_BAR_ROOM);
            for (ci, name) in res.columns.iter().enumerate() {
                let value = res.rows.cell(row, ci);
                let c = info[ci];
                // JSON objects and arrays can be read as a tree (the default) or as text.
                let json = value.and_then(|v| parsed_json(ui.ctx(), tab_id, ci, v));
                let tree_id = egui::Id::new(("json_tree", tab_id, ci));
                let as_text = ui.data(|d| d.get_temp::<bool>(tree_id)).unwrap_or(false);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if c.is_some_and(|c| c.pk) {
                        ui.label(theme::icon(icon::KEY).size(11.0).color(t.warning));
                    }
                    ui.label(RichText::new(name).size(11.0).color(t.muted_foreground));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if can_edit && let Some(fk) = c.and_then(|c| c.references.as_ref()) {
                            let pick = egui::Button::new(
                                theme::icon(icon::MAGNIFYING_GLASS)
                                    .size(11.0)
                                    .color(t.muted_foreground),
                            )
                            .frame(false);
                            if ui
                                .add(pick)
                                .on_hover_text(format!("Pick from {}", fk.table))
                                .clicked()
                            {
                                action = Some(DetailAction::PickFk(ci));
                            }
                        }
                        let open = egui::Button::new(
                            theme::icon(icon::ARROWS_OUT_SIMPLE)
                                .size(11.0)
                                .color(t.muted_foreground),
                        )
                        .frame(false);
                        // Only where a bigger editor helps: text (JSON, long values), not numbers,
                        // booleans or dates (Shift+Enter in the grid still opens any of them).
                        if kinds[ci] == ValueKind::Text
                            && ui
                                .add(open)
                                .on_hover_text("Open in the large editor (Shift+Enter in the grid)")
                                .clicked()
                        {
                            action = Some(DetailAction::Open(ci));
                        }
                        // Booleans and ENUMs have NULL inside their own selector below.
                        let nullable = c.is_none_or(|c| c.nullable);
                        if can_edit
                            && nullable
                            && value.is_some()
                            && kinds[ci] != ValueKind::Bool
                            && c.is_none_or(|c| c.enum_values.is_empty())
                        {
                            let null = egui::Button::new(RichText::new("NULL").size(10.0).color(t.subtle))
                                .frame(false);
                            if ui.add(null).on_hover_text("Set NULL").clicked() {
                                action = Some(DetailAction::Commit(ci, None));
                            }
                        }
                        if json.is_some() {
                            let (glyph, tip) = if as_text {
                                (icon::TREE_VIEW, "View as tree")
                            } else {
                                (icon::TEXT_T, "View as text (to edit)")
                            };
                            let b =
                                egui::Button::new(theme::icon(glyph).size(11.0).color(t.muted_foreground))
                                    .frame(false);
                            if ui.add(b).on_hover_text(tip).clicked() {
                                ui.data_mut(|d| d.insert_temp(tree_id, !as_text));
                            }
                        }
                        if let Some(c) = c {
                            ui.label(RichText::new(&c.data_type).size(10.0).color(t.subtle));
                        }
                    });
                });
                if let Some(v) = json.filter(|_| !as_text) {
                    egui::Frame::new()
                        .fill(t.sidebar)
                        .inner_margin(egui::Margin::symmetric(6, 4))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            json_tree(ui, egui::Id::new(("json", tab_id, row, ci)), None, "$", &v, 0);
                        });
                    continue;
                }
                if can_edit && kinds[ci] == ValueKind::Bool {
                    // Segmented true | false | NULL (NULL only when the column allows it).
                    let now = value.and_then(crate::sql::parse_bool);
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
                } else if can_edit && let Some(c) = c.filter(|c| !c.enum_values.is_empty()) {
                    // An ENUM: pick one of its values (NULL when the column allows it).
                    let mut chosen: Option<Option<String>> = None;
                    let shown = value.unwrap_or("NULL");
                    egui::ComboBox::from_id_salt(("enum", tab_id, row, ci))
                        .selected_text(RichText::new(shown).monospace().size(12.0))
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for v in &c.enum_values {
                                let on = value == Some(v.as_str());
                                if ui.selectable_label(on, RichText::new(v).monospace()).clicked() && !on {
                                    chosen = Some(Some(v.clone()));
                                }
                            }
                            if c.nullable {
                                let on = value.is_none();
                                let null = RichText::new("NULL").monospace().italics().color(t.subtle);
                                if ui.selectable_label(on, null).clicked() && !on {
                                    chosen = Some(None);
                                }
                            }
                        });
                    if let Some(v) = chosen {
                        action = Some(DetailAction::Commit(ci, v));
                    }
                } else if can_edit {
                    // The buffer is keyed by the row and current value, so it resets after the row
                    // is refreshed or another one is shown.
                    let key = egui::Id::new((row, value));
                    let original = value.unwrap_or_default().to_owned();
                    let mut text = ui
                        .data_mut(|d| {
                            let c = d.get_temp_mut_or_default::<DetailCache>(cache_id(tab_id));
                            c.text.get(&ci).filter(|(k, _)| *k == key).map(|(_, t)| t.clone())
                        })
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
                    ui.data_mut(|d| {
                        let c = d.get_temp_mut_or_default::<DetailCache>(cache_id(tab_id));
                        c.text.insert(ci, (key, text));
                    });
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
                    RichText::new("Edit and leave the field to save")
                        .size(11.0)
                        .color(t.subtle),
                );
            }
        });
    action
}
