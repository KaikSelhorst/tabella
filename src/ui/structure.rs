//! The "Structure" tab and the "new row" form.

use eframe::egui::{self, Align, Layout, RichText, TextEdit};

use super::{icon, syntax, theme};
use crate::app::{FieldMode, InsertField, StructState};
use crate::values::ValueKind;

pub enum StructAction {
    OpenSql(String),
    OpenData,
    /// A view: its definition in an editor.
    ViewSource,
    /// "Edit structure": open the table designer.
    Edit,
}

/// `is_view`: no designer and no table DDL (it would be a CREATE TABLE): the view's code.
pub fn structure_tab(ui: &mut egui::Ui, st: &StructState, busy: bool, is_view: bool) -> Option<StructAction> {
    let t = theme::tokens();
    let mut action = None;
    egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    theme::icon(icon::TREE_STRUCTURE)
                        .size(15.0)
                        .color(t.muted_foreground),
                );
                ui.label(
                    RichText::new(format!("{}.{}", st.schema, st.table))
                        .size(15.0)
                        .color(t.foreground),
                );
                if busy {
                    ui.spinner();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button(theme::icon_text(
                            icon::TABLE,
                            "Open data",
                            t.muted_foreground,
                            t.foreground,
                        ))
                        .clicked()
                    {
                        action = Some(StructAction::OpenData);
                    }
                    if is_view {
                        let code =
                            theme::icon_text(icon::CODE, "View code", t.muted_foreground, t.foreground);
                        if ui.button(code).clicked() {
                            action = Some(StructAction::ViewSource);
                        }
                    }
                    if st.data.is_some()
                        && !is_view
                        && ui
                            .button(theme::icon_text(
                                icon::PENCIL_SIMPLE,
                                "Edit structure",
                                t.muted_foreground,
                                t.foreground,
                            ))
                            .clicked()
                    {
                        action = Some(StructAction::Edit);
                    }
                    if let Some(d) = st.data.as_ref().filter(|_| !is_view) {
                        let open = theme::icon_text(
                            icon::CODE,
                            "Open DDL in editor",
                            t.muted_foreground,
                            t.foreground,
                        );
                        if ui.button(open).clicked() {
                            action = Some(StructAction::OpenSql(d.ddl.clone()));
                        }
                        if ui
                            .button(theme::icon_text(
                                icon::COPY,
                                "Copy DDL",
                                t.muted_foreground,
                                t.foreground,
                            ))
                            .clicked()
                        {
                            ui.ctx().copy_text(d.ddl.clone());
                        }
                    }
                });
            });
        });
    let Some(d) = &st.data else { return action };

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 4))
                .show(ui, |ui| {
                    section(ui, &format!("Columns  {}", d.columns.len()));
                    egui::Grid::new("struct_cols")
                        .num_columns(5)
                        .spacing([16.0, 6.0])
                        .striped(false)
                        .show(ui, |ui| {
                            for h in ["", "Name", "Type", "Null", "Default"] {
                                ui.label(RichText::new(h).size(11.0).color(t.subtle));
                            }
                            ui.end_row();
                            for c in &d.columns {
                                if c.pk {
                                    ui.label(theme::icon(icon::KEY).size(12.0).color(t.warning))
                                        .on_hover_text("Primary key");
                                } else {
                                    ui.label("");
                                }
                                ui.label(RichText::new(&c.name).color(t.foreground));
                                ui.label(RichText::new(&c.data_type).monospace().size(12.0).color(t.number));
                                ui.label(
                                    RichText::new(if c.nullable { "yes" } else { "no" })
                                        .color(t.muted_foreground),
                                );
                                match &c.default {
                                    Some(v) => ui.label(
                                        RichText::new(v).monospace().size(12.0).color(t.muted_foreground),
                                    ),
                                    None => ui.label(RichText::new("—").color(t.subtle)),
                                };
                                ui.end_row();
                            }
                        });

                    section(ui, &format!("Indexes  {}", d.indexes.len()));
                    named_list(ui, &d.indexes, "No indexes");
                    section(ui, &format!("Foreign keys  {}", d.foreign_keys.len()));
                    named_list(ui, &d.foreign_keys, "No foreign keys");

                    // A view's "DDL" would be a CREATE TABLE made from its columns: misleading.
                    if is_view {
                        ui.add_space(12.0);
                        return;
                    }
                    section(ui, "DDL");
                    egui::Frame::new()
                        .fill(t.sidebar)
                        .inner_margin(egui::Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let job = syntax::highlight(&d.ddl, &egui::FontId::monospace(12.5));
                            ui.add(egui::Label::new(job).selectable(true));
                        });
                    ui.add_space(12.0);
                });
        });
    action
}

fn section(ui: &mut egui::Ui, title: &str) {
    let t = theme::tokens();
    ui.add_space(14.0);
    ui.label(RichText::new(title.to_uppercase()).size(11.0).color(t.subtle));
    ui.add_space(4.0);
}

fn named_list(ui: &mut egui::Ui, items: &[(String, String)], empty: &str) {
    let t = theme::tokens();
    if items.is_empty() {
        ui.label(RichText::new(empty).color(t.subtle));
    }
    for (name, def) in items {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(name).color(t.foreground));
            ui.label(
                RichText::new(def)
                    .monospace()
                    .size(12.0)
                    .color(t.muted_foreground),
            );
        });
    }
}

pub enum InsertAction {
    Submit,
    Cancel,
    /// Choose field `i` (a foreign key) from the referenced rows.
    PickFk(usize),
}

/// Modal "New row": each column is left to its default, set to NULL, or given a value.
pub fn insert_form(
    ctx: &egui::Context,
    tab_id: u64,
    table: &str,
    fields: &mut [InsertField],
) -> Option<InsertAction> {
    let t = theme::tokens();
    let mut action = None;
    let modal = egui::Modal::new(egui::Id::new(("insert_form", tab_id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(520.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::PLUS).size(16.0).color(t.lime));
                ui.label(
                    RichText::new(format!("New row in {table}"))
                        .size(15.0)
                        .color(t.foreground),
                );
            });
            ui.label(
                RichText::new(
                    "\"Default\" lets the database fill it in (DEFAULT, sequence, auto_increment).",
                )
                .size(11.0)
                .color(t.subtle),
            );
            ui.add_space(8.0);
            // Short forms draw directly. Long ones scroll in a box of fixed height: a scroll area
            // that sizes itself to its content inside a modal that sizes itself to *its* content
            // kept resizing and made the whole app repaint non-stop.
            let pick = if fields.len() <= SCROLL_AFTER {
                insert_rows(ui, tab_id, fields)
            } else {
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| insert_rows(ui, tab_id, fields))
                    .inner
            };
            if let Some(i) = pick {
                action = Some(InsertAction::PickFk(i));
            }
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(theme::primary_button(Some(icon::PLUS), "Insert"))
                    .clicked()
                {
                    action = Some(InsertAction::Submit);
                }
                if ui.add(theme::outline_button("Cancel")).clicked() {
                    action = Some(InsertAction::Cancel);
                }
            });
        });
    if modal.should_close() && action.is_none() {
        action = Some(InsertAction::Cancel);
    }
    action
}

/// Width of the column with names and types in the "new row" form.
const NAME_WIDTH: f32 = 150.0;

/// Forms with more columns than this get a scroll box.
const SCROLL_AFTER: usize = 10;

/// The form's rows; returns a foreign-key field whose "escolher" was clicked.
fn insert_rows(ui: &mut egui::Ui, tab_id: u64, fields: &mut [InsertField]) -> Option<usize> {
    let t = theme::tokens();
    let mut pick = None;
    egui::Grid::new(("insert_grid", tab_id))
        .num_columns(3)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            for (i, f) in fields.iter_mut().enumerate() {
                ui.vertical(|ui| {
                    // Fixed width and no wrapping: long names and types are cut with "…", not broken
                    // letter by letter.
                    ui.set_width(NAME_WIDTH);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add(egui::Label::new(RichText::new(&f.name).color(t.foreground)).truncate());
                    ui.add(
                        egui::Label::new(RichText::new(&f.data_type).size(10.0).color(t.subtle)).truncate(),
                    );
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (mode, label) in [
                        (FieldMode::Default, "Default"),
                        (FieldMode::Null, "NULL"),
                        (FieldMode::Value, "Value"),
                    ] {
                        let enabled = mode != FieldMode::Null || f.nullable;
                        let b = egui::Button::new(RichText::new(label).size(11.0)).selected(f.mode == mode);
                        if ui.add_enabled(enabled, b).clicked() {
                            f.mode = mode;
                        }
                    }
                });
                if f.mode == FieldMode::Value {
                    ui.horizontal(|ui| {
                        value_editor(ui, f);
                        if let Some(fk) = &f.references {
                            let b = egui::Button::new(
                                theme::icon(icon::MAGNIFYING_GLASS).color(t.muted_foreground),
                            );
                            if ui
                                .add(b)
                                .on_hover_text(format!("Pick from {}", fk.table))
                                .clicked()
                            {
                                pick = Some(i);
                            }
                        }
                    });
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
    pick
}

fn value_editor(ui: &mut egui::Ui, f: &mut InsertField) {
    let t = theme::tokens();
    if f.kind == ValueKind::Bool {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for v in ["true", "false"] {
                let on = f.text == v;
                let color = if on { t.boolean } else { t.subtle };
                if ui
                    .add(egui::Button::new(RichText::new(v).monospace().color(color)).selected(on))
                    .clicked()
                {
                    f.text = v.to_string();
                }
            }
        });
    } else if !f.enum_values.is_empty() {
        let shown = if f.text.is_empty() {
            "choose…"
        } else {
            f.text.as_str()
        };
        egui::ComboBox::from_id_salt(("enum_field", &f.name))
            .selected_text(RichText::new(shown).monospace().size(12.5))
            .width(220.0)
            .show_ui(ui, |ui| {
                for v in &f.enum_values {
                    ui.selectable_value(&mut f.text, v.clone(), RichText::new(v).monospace());
                }
            });
    } else {
        ui.add(
            TextEdit::singleline(&mut f.text)
                .font(egui::FontId::monospace(12.5))
                .desired_width(220.0),
        );
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;

    use super::*;

    fn fields(n: usize) -> Vec<InsertField> {
        (0..n)
            .map(|i| InsertField {
                name: format!("column_{i}"),
                references: None,
                data_type: "text".into(),
                kind: ValueKind::Text,
                enum_values: Vec::new(),
                nullable: true,
                mode: if i % 3 == 0 {
                    FieldMode::Value
                } else {
                    FieldMode::Default
                },
                text: String::new(),
            })
            .collect()
    }

    /// `Harness::run` fails when the UI keeps requesting repaints; the form must settle both
    /// with and without its scroll box.
    #[test]
    fn insert_form_settles() {
        for n in [3, SCROLL_AFTER + 15] {
            let mut f = fields(n);
            let mut themed = false;
            let mut harness = Harness::builder()
                .with_size(egui::vec2(1280.0, 760.0))
                .build_ui(move |ui| {
                    // Fonts (the icon family) are installed on the first frame and usable from the next.
                    if !themed {
                        theme::apply(ui.ctx());
                        themed = true;
                        return;
                    }
                    insert_form(ui.ctx(), 1, "public.t", &mut f);
                });
            harness.run();
        }
    }
}
