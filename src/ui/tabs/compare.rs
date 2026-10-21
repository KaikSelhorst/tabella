//! "Compare structure": pick both sides, see what differs table by table, take the SQL.

use super::*;
use crate::compare::Status;

pub(super) fn compare_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let t = theme::tokens();
    let id = tab.id;
    if let Some(e) = &tab.error {
        error_banner(ui, e);
    }
    let TabKind::Compare(st) = &mut tab.kind else {
        return;
    };
    let pg = s.cfg.kind == crate::db::Kind::Postgres;
    // Schemas of this connection (MySQL: its databases), for the source and a same-connection target.
    let schemas: Vec<String> = if pg {
        let mut v: Vec<String> = s.tables.iter().map(|t| t.schema.clone()).collect();
        v.dedup();
        v
    } else {
        s.databases.clone()
    };
    let (mut start, mut open_sql, mut save) = (false, false, false);
    egui::ScrollArea::vertical()
        .id_salt(("compare", id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .fill(t.background)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::icon(icon::GIT_DIFF).size(15.0).color(t.muted_foreground));
                        ui.label(egui::RichText::new("Compare structure").size(15.0).color(t.foreground));
                    });
                    ui.label(
                        egui::RichText::new("The generated SQL gives the target the source's structure. Data isn't touched.")
                            .size(11.0)
                            .color(t.subtle),
                    );
                    ui.add_space(8.0);
                    let side = if pg { "schema" } else { "database" };
                    egui::Grid::new(("compare_sides", id))
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("Source").color(t.muted_foreground));
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&s.cfg.name).color(t.foreground));
                                if pg {
                                    ui.label(egui::RichText::new(format!("· {} ·", s.current_db)).color(t.subtle));
                                }
                                egui::ComboBox::from_id_salt(("compare_src", id))
                                    .selected_text(&st.source_schema)
                                    .width(180.0)
                                    .show_ui(ui, |ui| {
                                        for sc in &schemas {
                                            ui.selectable_value(&mut st.source_schema, sc.clone(), sc);
                                        }
                                    });
                            });
                            ui.end_row();

                            ui.label(egui::RichText::new("Target").color(t.muted_foreground));
                            ui.horizontal(|ui| {
                                let current = cx
                                    .saved
                                    .iter()
                                    .find(|c| c.id == st.target_conn)
                                    .map_or(s.cfg.name.clone(), |c| c.name.clone());
                                egui::ComboBox::from_id_salt(("compare_conn", id))
                                    .selected_text(current)
                                    .width(160.0)
                                    .show_ui(ui, |ui| {
                                        for c in cx.saved.iter().filter(|c| c.kind == s.cfg.kind) {
                                            if ui.selectable_label(st.target_conn == c.id, &c.name).clicked() {
                                                st.target_conn = c.id.clone();
                                                st.target_db = if c.id == s.cfg.id {
                                                    s.current_db.clone()
                                                } else {
                                                    c.database.clone()
                                                };
                                            }
                                        }
                                    });
                                let same_conn = st.target_conn == s.cfg.id;
                                if pg {
                                    ui.label(egui::RichText::new("database").color(t.subtle));
                                    ui.add(TextEdit::singleline(&mut st.target_db).desired_width(110.0));
                                }
                                ui.label(egui::RichText::new(side).color(t.subtle));
                                // Here the names are known; elsewhere they are typed.
                                if same_conn && (!pg || st.target_db == s.current_db) {
                                    egui::ComboBox::from_id_salt(("compare_dst", id))
                                        .selected_text(if st.target_schema.is_empty() { "choose…" } else { &st.target_schema })
                                        .width(160.0)
                                        .show_ui(ui, |ui| {
                                            for sc in schemas.iter().filter(|sc| **sc != st.source_schema) {
                                                ui.selectable_value(&mut st.target_schema, sc.clone(), sc);
                                            }
                                        });
                                } else {
                                    ui.add(
                                        TextEdit::singleline(&mut st.target_schema)
                                            .hint_text(if pg { "public" } else { "database name" })
                                            .desired_width(140.0),
                                    );
                                }
                            });
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().interact_size.y = 28.0;
                        let ready = !st.running && !st.target_schema.trim().is_empty();
                        if ui
                            .add_enabled(ready, theme::primary_button(Some(icon::GIT_DIFF), "Compare"))
                            .on_disabled_hover_text(format!("Choose the target {side}"))
                            .clicked()
                        {
                            start = true;
                        }
                        if st.running {
                            ui.spinner();
                            ui.label(egui::RichText::new("Reading both structures…").color(t.muted_foreground));
                        }
                    });
                    let Some(result) = &st.result else { return };
                    ui.add_space(12.0);
                    let diffs = match result {
                        Err(e) => return error_banner(ui, e),
                        Ok(d) => d,
                    };
                    ui.label(egui::RichText::new(&st.compared).monospace().color(t.foreground));
                    if diffs.is_empty() {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(theme::icon(icon::CHECK).size(16.0).color(t.accent_text));
                            ui.label(egui::RichText::new("Same structure on both sides").color(t.foreground));
                        });
                        return;
                    }
                    let count = |s: Status| diffs.iter().filter(|d| d.status == s).count();
                    ui.label(
                        egui::RichText::new(format!(
                            "{} different · {} new · {} only in the target",
                            count(Status::Changed),
                            count(Status::New),
                            count(Status::Extra)
                        ))
                        .size(12.0)
                        .color(t.muted_foreground),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if st.same_connection {
                            if ui.add(theme::primary_button(Some(icon::CODE), "Open SQL in editor")).clicked() {
                                open_sql = true;
                            }
                        } else {
                            if ui
                                .add(theme::primary_button(Some(icon::FLOPPY_DISK), "Save .sql"))
                                .on_hover_text("In the Downloads folder; run it on the target with \"Run .sql file\"")
                                .clicked()
                            {
                                save = true;
                            }
                            if ui.add(theme::outline_button("Copy SQL")).clicked() {
                                ui.ctx().copy_text(app::compare_script(st));
                            }
                        }
                    });
                    if !st.same_connection {
                        ui.label(
                            egui::RichText::new(
                                "The target is another connection: the SQL doesn't open here so it doesn't run on the wrong database. \
                                 Connect to the target and use \"Run .sql file\" (or paste it into an editor there).",
                            )
                            .size(11.0)
                            .color(t.warning),
                        );
                    }
                    if let Some(path) = &st.saved {
                        ui.horizontal(|ui| {
                            ui.label(theme::icon(icon::CHECK).color(t.accent_text));
                            ui.label(egui::RichText::new(path.display().to_string()).monospace().size(12.0));
                            if ui.small_button("Show in folder").clicked() {
                                export::reveal(path);
                            }
                        });
                    }
                    ui.add_space(8.0);
                    for d in diffs {
                        let (badge, color) = match d.status {
                            Status::Changed => ("changed", t.info),
                            Status::New => ("new", t.accent_text),
                            Status::Extra => ("only in target", t.warning),
                        };
                        let header = egui::RichText::new(format!("{}  ·  {badge}", d.table)).color(color);
                        egui::CollapsingHeader::new(header)
                            .id_salt(("compare_table", id, &d.table))
                            .default_open(d.status == Status::Changed)
                            .show(ui, |ui| {
                                for it in &d.items {
                                    let color = if it.destructive { t.destructive } else { t.foreground };
                                    ui.label(egui::RichText::new(format!("• {}", it.what)).color(color))
                                        .on_hover_text(egui::RichText::new(&it.sql).monospace());
                                }
                            });
                    }
                });
        });
    if start {
        app::start_compare(tab, s, cx.saved);
    }
    if open_sql && let TabKind::Compare(st) = &tab.kind {
        cx.request = Some(Request::Sql(app::compare_script(st)));
    }
    if save && let TabKind::Compare(st) = &mut tab.kind {
        let path = export::target_path("comparison", Format::Sql);
        match std::fs::write(&path, app::compare_script(st)) {
            Ok(()) => {
                st.saved = Some(path);
                tab.error = None;
            }
            Err(e) => tab.error = Some(format!("Couldn't save: {e}")),
        }
    }
}
