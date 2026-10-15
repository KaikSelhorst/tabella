//! "New table" / "Edit structure": columns and indexes as a form, with the DDL it produces
//! shown live underneath.

use eframe::egui::{self, RichText, TextEdit};

use super::{icon, syntax, theme};
use crate::db::Kind;
use crate::design::{ColDef, Design, IndexDef, common_types};

pub enum DesignAction {
    Run(String),
    OpenSql(String),
}

const NAME_W: f32 = 168.0;
const TYPE_W: f32 = 148.0;
const DEFAULT_W: f32 = 148.0;

pub fn design_tab(ui: &mut egui::Ui, tab_id: u64, d: &mut Design, busy: bool) -> Option<DesignAction> {
    let t = theme::tokens();
    let mut action = None;
    egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::TABLE).size(15.0).color(t.muted_foreground));
                if d.existing {
                    ui.label(
                        RichText::new(format!("Edit {}.{}", d.schema, d.table))
                            .size(15.0)
                            .color(t.foreground),
                    );
                } else {
                    ui.label(RichText::new("New table").size(15.0).color(t.foreground));
                    ui.add_space(8.0);
                    ui.add(
                        TextEdit::singleline(&mut d.schema)
                            .hint_text(if d.kind == Kind::MySql {
                                "database"
                            } else {
                                "schema"
                            })
                            .font(egui::FontId::monospace(12.5))
                            .desired_width(120.0),
                    );
                    ui.label(RichText::new(".").color(t.subtle));
                    ui.add(
                        TextEdit::singleline(&mut d.table)
                            .hint_text("table_name")
                            .font(egui::FontId::monospace(12.5))
                            .desired_width(200.0),
                    );
                }
                if busy {
                    ui.spinner();
                }
            });
        });

    egui::ScrollArea::vertical()
        .id_salt(("design", tab_id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 4))
                .show(ui, |ui| {
                    section(ui, "Columns");
                    columns(ui, tab_id, d);
                    ui.add_space(4.0);
                    if ui
                        .button(theme::icon_text(icon::PLUS, "Column", t.lime, t.foreground))
                        .clicked()
                    {
                        d.columns.push(ColDef::new());
                    }

                    section(ui, "Indexes");
                    indexes(ui, tab_id, d);
                    ui.add_space(4.0);
                    if ui
                        .button(theme::icon_text(icon::PLUS, "Index", t.lime, t.foreground))
                        .clicked()
                    {
                        d.indexes.push(IndexDef {
                            existing: false,
                            name: String::new(),
                            columns: String::new(),
                            unique: false,
                            dropped: false,
                        });
                    }

                    section(ui, "SQL");
                    let problems = d.problems();
                    let script = d.script();
                    egui::Frame::new()
                        .fill(t.sidebar)
                        .inner_margin(egui::Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if script.is_empty() {
                                ui.label(RichText::new("Nothing changed yet").color(t.subtle));
                            } else {
                                let job = syntax::highlight(&script, egui::FontId::monospace(12.5));
                                ui.add(egui::Label::new(job).selectable(true));
                            }
                        });
                    for p in &problems {
                        ui.label(RichText::new(format!("• {p}")).color(t.destructive));
                    }
                    if d.kind == Kind::MySql && d.existing && d.statements().len() > 1 {
                        ui.label(
                            RichText::new(
                                "In MySQL each ALTER applies immediately: if one fails, the earlier ones stay.",
                            )
                            .size(11.0)
                            .color(t.warning),
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let ready = problems.is_empty() && !script.is_empty() && !busy;
                        let label = if d.existing {
                            "Apply changes"
                        } else {
                            "Create table"
                        };
                        if ui
                            .add_enabled(ready, theme::primary_button(Some(icon::PLAY), label))
                            .clicked()
                        {
                            action = Some(DesignAction::Run(script.clone()));
                        }
                        if ui
                            .add_enabled(
                                !script.is_empty(),
                                egui::Button::new(theme::icon_text(
                                    icon::CODE,
                                    "Open in editor",
                                    t.muted_foreground,
                                    t.foreground,
                                )),
                            )
                            .on_hover_text("To review or tweak the SQL before running it")
                            .clicked()
                        {
                            action = Some(DesignAction::OpenSql(script.clone()));
                        }
                    });
                    ui.add_space(12.0);
                });
        });
    action
}

fn section(ui: &mut egui::Ui, title: &str) {
    let t = theme::tokens();
    ui.add_space(12.0);
    ui.label(RichText::new(title.to_uppercase()).size(11.0).color(t.subtle));
    ui.add_space(4.0);
}

fn columns(ui: &mut egui::Ui, tab_id: u64, d: &mut Design) {
    let t = theme::tokens();
    let kind = d.kind;
    let existing = d.existing;
    let mut remove = None;
    egui::Grid::new(("design_cols", tab_id))
        .num_columns(6)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            for h in ["PK", "Name", "Type", "Null", "Default", ""] {
                ui.label(RichText::new(h).size(11.0).color(t.subtle));
            }
            ui.end_row();
            for (i, c) in d.columns.iter_mut().enumerate() {
                let live = !c.dropped;
                // The primary key of an existing table isn't changed here (it needs the
                // constraint's name and usually a migration plan).
                ui.add_enabled(live && !existing, egui::Checkbox::without_text(&mut c.pk))
                    .on_disabled_hover_text("An existing table's primary key isn't changed here");
                let mono = egui::FontId::monospace(12.5);
                // Fixed sizes: inside a grid a text field otherwise shrinks to the column's
                // starting width and stays there.
                ui.add_enabled_ui(live, |ui| {
                    ui.add_sized(
                        [NAME_W, 22.0],
                        TextEdit::singleline(&mut c.name)
                            .font(mono.clone())
                            .hint_text("column"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_enabled_ui(live, |ui| {
                        ui.add_sized(
                            [TYPE_W, 22.0],
                            TextEdit::singleline(&mut c.data_type)
                                .font(mono.clone())
                                .hint_text("type"),
                        );
                    });
                    ui.add_enabled_ui(live, |ui| {
                        ui.menu_button(theme::icon(icon::CARET_DOWN).color(t.muted_foreground), |ui| {
                            for ty in common_types(kind) {
                                if ui.button(RichText::new(*ty).monospace()).clicked() {
                                    c.data_type = ty.to_string();
                                    ui.close();
                                }
                            }
                        });
                    });
                });
                ui.add_enabled(live && !c.pk, egui::Checkbox::without_text(&mut c.nullable));
                ui.add_enabled_ui(live, |ui| {
                    ui.add_sized(
                        [DEFAULT_W, 22.0],
                        TextEdit::singleline(&mut c.default)
                            .font(mono)
                            .hint_text("no default"),
                    );
                });
                let (glyph, tip) = if c.dropped {
                    (icon::ARROW_COUNTER_CLOCKWISE, "Keep the column")
                } else {
                    (icon::TRASH, "Delete the column")
                };
                let color = if c.dropped { t.lime } else { t.muted_foreground };
                if ui
                    .add(egui::Button::new(theme::icon(glyph).color(color)).frame(false))
                    .on_hover_text(tip)
                    .clicked()
                {
                    if c.original.is_some() {
                        c.dropped = !c.dropped;
                    } else {
                        remove = Some(i);
                    }
                }
                if c.pk {
                    c.nullable = false;
                }
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        d.columns.remove(i);
    }
}

fn indexes(ui: &mut egui::Ui, tab_id: u64, d: &mut Design) {
    let t = theme::tokens();
    if d.indexes.is_empty() {
        ui.label(RichText::new("No index besides the primary key").color(t.subtle));
        return;
    }
    let mut remove = None;
    egui::Grid::new(("design_ix", tab_id))
        .num_columns(4)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            for (i, ix) in d.indexes.iter_mut().enumerate() {
                if ix.existing {
                    let color = if ix.dropped { t.subtle } else { t.foreground };
                    let mut name = RichText::new(&ix.name).color(color);
                    if ix.dropped {
                        name = name.strikethrough();
                    }
                    ui.label(name);
                    ui.add_sized(
                        [260.0, 22.0],
                        egui::Label::new(
                            RichText::new(&ix.columns)
                                .monospace()
                                .size(11.0)
                                .color(t.muted_foreground),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&ix.columns);
                    ui.label("");
                } else {
                    ui.add_sized(
                        [NAME_W, 22.0],
                        TextEdit::singleline(&mut ix.name)
                            .hint_text("name (optional)")
                            .font(egui::FontId::monospace(12.5)),
                    );
                    ui.add_sized(
                        [260.0, 22.0],
                        TextEdit::singleline(&mut ix.columns)
                            .hint_text("column1, column2")
                            .font(egui::FontId::monospace(12.5)),
                    );
                    ui.checkbox(&mut ix.unique, "unique");
                }
                let (glyph, tip) = if ix.dropped {
                    (icon::ARROW_COUNTER_CLOCKWISE, "Keep the index")
                } else {
                    (icon::TRASH, "Delete the index")
                };
                if ui
                    .add(egui::Button::new(theme::icon(glyph).color(t.muted_foreground)).frame(false))
                    .on_hover_text(tip)
                    .clicked()
                {
                    if ix.existing {
                        ix.dropped = !ix.dropped;
                    } else {
                        remove = Some(i);
                    }
                }
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        d.indexes.remove(i);
    }
}
