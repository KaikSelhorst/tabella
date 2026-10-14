use eframe::egui::{self, TextEdit};

use super::{icon, palette, theme};
use crate::app::{App, ConnForm, TabKind};
use crate::db::{ConnConfig, DbObject, Kind, ObjKind, TableRef};
use crate::worker::Job;

/// Schemas with fewer tables than this start expanded.
const AUTO_EXPAND_BELOW: usize = 200;

impl App {
    pub fn side_panel(&mut self, ui: &mut egui::Ui) {
        self.connection_list(ui);
        self.location_row(ui);
        ui.separator();
        self.table_tree(ui);
    }

    /// Where unqualified names in the editor go: the database (MySQL `USE`, or another
    /// Postgres database through a reconnect) and, on Postgres, the schema.
    fn location_row(&mut self, ui: &mut egui::Ui) {
        let Some(s) = self
            .session
            .as_ref()
            .filter(|s| s.ready && !s.current_db.is_empty())
        else {
            return;
        };
        let tk = theme::tokens();
        let pg = s.cfg.kind == Kind::Postgres;
        let mut schemas: Vec<String> = s.tables.iter().map(|t| t.schema.clone()).collect();
        schemas.dedup();
        if !schemas.contains(&s.current_schema) && !s.current_schema.is_empty() {
            schemas.insert(0, s.current_schema.clone());
        }
        let (dbs, db, schema) = (
            s.databases.clone(),
            s.current_db.clone(),
            s.current_schema.clone(),
        );
        let mut pick_db: Option<String> = None;
        let mut pick_schema: Option<String> = None;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let combo =
                |ui: &mut egui::Ui, id: &str, current: &str, items: &[String], width: f32, tip: &str| {
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt(id)
                        .selected_text(egui::RichText::new(current).monospace().size(12.0))
                        .width(width)
                        .show_ui(ui, |ui| {
                            for it in items {
                                if ui
                                    .selectable_label(it == current, egui::RichText::new(it).monospace())
                                    .clicked()
                                    && it != current
                                {
                                    chosen = Some(it.clone());
                                }
                            }
                        })
                        .response
                        .on_hover_text(tip);
                    chosen
                };
            ui.label(theme::icon(icon::DATABASE).size(12.0).color(tk.subtle));
            let half = (ui.available_width() - 28.0) / 2.0;
            let db_tip = if pg {
                "Banco: trocar reconecta no mesmo servidor (as abas desta conexão fecham)"
            } else {
                "Banco onde os nomes sem prefixo são procurados (USE)"
            };
            pick_db = combo(
                ui,
                "loc_db",
                &db,
                &dbs,
                if pg { half } else { ui.available_width() - 8.0 },
                db_tip,
            );
            if pg {
                ui.label(egui::RichText::new(".").color(tk.subtle));
                pick_schema = combo(
                    ui,
                    "loc_schema",
                    &schema,
                    &schemas,
                    ui.available_width() - 8.0,
                    "Schema onde os nomes sem prefixo são procurados primeiro (search_path)",
                );
            }
        });
        if let Some(db) = pick_db {
            self.switch_location(Some(&db), None);
        }
        if let Some(sc) = pick_schema {
            self.switch_location(None, Some(&sc));
        }
    }

    fn connection_list(&mut self, ui: &mut egui::Ui) {
        let current = self
            .session
            .as_ref()
            .and_then(|s| self.saved.iter().position(|c| c.id == s.cfg.id));
        let mut connect = None;
        let mut edit = None;
        let mut delete = None;
        let mut add = false;
        let mut manual: Option<bool> = None;
        let session_manual = self.session.as_ref().map(|s| s.txn.manual);

        // Right to left inside one row: the menu button first, then the combo box fills what is
        // left. (Without the row, the layout would center itself in the whole panel height.)
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let selected: egui::WidgetText = match current {
                    // The label follows this session's mode, which can differ from the saved one.
                    Some(i) => conn_label(&ConnConfig {
                        manual_commit: session_manual.unwrap_or(self.saved[i].manual_commit),
                        ..self.saved[i].clone()
                    })
                    .into(),
                    None => egui::RichText::new("Selecionar conexão…")
                        .color(theme::tokens().muted_foreground)
                        .into(),
                };
                ui.add_enabled_ui(current.is_some(), |ui| {
                    ui.menu_button(theme::icon(icon::DOTS_THREE), |ui| {
                        if ui.button("Reconectar").clicked() {
                            connect = current;
                            ui.close();
                        }
                        if ui.button("Editar").clicked() {
                            edit = current;
                            ui.close();
                        }
                        if ui.button("Excluir").clicked() {
                            delete = current;
                            ui.close();
                        }
                        if let Some(mut on) = session_manual {
                            ui.separator();
                            if ui
                                .checkbox(&mut on, "Commit manual nesta sessão")
                                .on_hover_text(
                                    "Alterações ficam numa transação até você confirmar ou desfazer",
                                )
                                .changed()
                            {
                                manual = Some(on);
                                ui.close();
                            }
                        }
                    });
                });
                egui::ComboBox::from_id_salt("connection")
                    .selected_text(selected)
                    // Exactly the space left after the menu button: a guessed width made the row a
                    // few pixels wider than the panel, and the panel grew by that much every frame.
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for (i, c) in self.saved.iter().enumerate() {
                            let r = ui.selectable_label(current == Some(i), conn_label(c));
                            if r.clicked() {
                                connect = Some(i);
                                ui.close();
                            }
                            r.context_menu(|ui| {
                                if ui.button("Editar").clicked() {
                                    edit = Some(i);
                                    ui.close();
                                }
                                if ui.button("Excluir").clicked() {
                                    delete = Some(i);
                                    ui.close();
                                }
                            });
                        }
                        if !self.saved.is_empty() {
                            ui.separator();
                        }
                        if ui
                            .selectable_label(
                                false,
                                theme::icon_text(
                                    icon::PLUS,
                                    "Adicionar conexão…",
                                    theme::tokens().lime,
                                    theme::tokens().foreground,
                                ),
                            )
                            .clicked()
                        {
                            add = true;
                            ui.close();
                        }
                    });
            });
        });

        // Indices are only valid within this frame; from here on connections go by id.
        if add {
            self.form = Some(ConnForm {
                editing: None,
                cfg: ConnConfig::new(Kind::Postgres),
            });
        }
        if let Some(id) = connect.map(|i| self.saved[i].id.clone()) {
            self.connect_saved(&id);
        }
        if let Some(c) = edit.map(|i| self.saved[i].clone()) {
            self.form = Some(ConnForm {
                editing: Some(c.id.clone()),
                cfg: c,
            });
        }
        if let Some(id) = delete.map(|i| self.saved[i].id.clone()) {
            self.delete_connection(&id);
        }
        if let Some(on) = manual {
            self.set_manual_commit(on);
        }
    }

    fn table_tree(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.session else {
            ui.label(
                egui::RichText::new("Sem conexão ativa")
                    .size(11.0)
                    .color(theme::tokens().subtle),
            );
            if let Some(n) = &self.notice {
                let t = theme::tokens();
                let color = if n.error { t.destructive } else { t.subtle };
                ui.add(egui::Label::new(egui::RichText::new(&n.text).small().color(color)).wrap());
            }
            return;
        };
        if !s.ready {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Conectando…");
            });
            return;
        }
        let mut disconnect = false;
        let mut open_activity = false;
        let mut new_table = false;
        let mut schema_action: Option<(String, bool)> = None;
        let mut sort_by_size_toggle = false;
        let mut open_sql = false;
        let tk = theme::tokens();
        ui.add_space(4.0);
        // Section header with quiet icon actions, like a file tree.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("TABELAS").size(11.0).color(tk.subtle));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let action = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
                    ui.add(egui::Button::new(theme::icon(glyph).color(tk.muted_foreground)).frame(false))
                        .on_hover_text(tip)
                        .clicked()
                };
                disconnect = action(ui, icon::PLUGS, "Desconectar");
                if action(ui, icon::PULSE, "Atividade no servidor") {
                    open_activity = true;
                }
                if !s.cfg.read_only && action(ui, icon::PLUS, "Nova tabela") {
                    new_table = true;
                }
                let sort = theme::icon(icon::SORT_DESCENDING).color(if self.sort_by_size {
                    tk.lime
                } else {
                    tk.muted_foreground
                });
                if ui
                    .add(egui::Button::new(sort).frame(false))
                    .on_hover_text(if self.sort_by_size {
                        "Ordenar por nome"
                    } else {
                        "Ordenar por tamanho"
                    })
                    .clicked()
                {
                    sort_by_size_toggle = true;
                }
                if action(ui, icon::ARROWS_CLOCKWISE, "Recarregar tabelas") {
                    s.send(Job::Tables);
                }
                open_sql = action(ui, icon::CODE, "Novo editor SQL");
            });
        });
        ui.add(
            TextEdit::singleline(&mut self.filter)
                .hint_text("Filtrar tabelas…")
                .margin(egui::Margin::symmetric(8, 5))
                .desired_width(ui.available_width()),
        );

        let active_table = self.tabs.get(self.active).and_then(|t| match &t.kind {
            TabKind::Table(ts) => Some((ts.schema.as_str(), ts.table.as_str())),
            TabKind::Structure(st) => Some((st.schema.as_str(), st.table.as_str())),
            TabKind::Sql(_)
            | TabKind::Activity(_)
            | TabKind::Design(_)
            | TabKind::Search(_)
            | TabKind::Diagram(_)
            | TabKind::History(_)
            | TabKind::Script(_)
            | TabKind::Dump(_)
            | TabKind::Compare(_) => None,
        });
        let needle = self.filter.to_lowercase();
        let by_size = self.sort_by_size;
        let kind = s.cfg.kind;
        let mut to_open: Option<(String, String)> = None;
        let mut to_sql: Option<String> = None;
        let mut to_template: Option<(String, String, crate::sql::Template)> = None;
        let mut to_object: Option<DbObject> = None;
        let mut to_run: Option<DbObject> = None;
        let mut to_action: Option<crate::app::TableActionPrompt> = None;
        let mut to_dump: Option<crate::app::DumpScope> = None;
        let mut to_compare: Option<String> = None;
        let mut to_structure: Option<(String, String)> = None;
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.spacing_mut().button_padding = egui::vec2(6.0, 2.0);
                // Where unqualified names go (the Postgres schema, the MySQL database) comes
                // first and is marked.
                let default_schema = match kind {
                    crate::db::Kind::Postgres => s.current_schema.as_str(),
                    crate::db::Kind::MySql => s.current_db.as_str(),
                };
                let mut groups: Vec<&[TableRef]> = s.tables.chunk_by(|a, b| a.schema == b.schema).collect();
                groups.sort_by_key(|g| g[0].schema != default_schema);
                let focus = self.schema_focus.take();
                for group in groups {
                    let mut visible: Vec<&TableRef> = group
                        .iter()
                        .filter(|t| needle.is_empty() || t.search_key.contains(&needle))
                        .collect();
                    if by_size {
                        visible.sort_by_key(|t| std::cmp::Reverse(t.size.unwrap_or(-1)));
                    }
                    if visible.is_empty() {
                        continue;
                    }
                    let schema = &group[0].schema;
                    let is_default = schema == default_schema;
                    let header = if is_default {
                        egui::RichText::new(format!("{schema}  {}  · padrão", visible.len()))
                            .size(11.0)
                            .color(tk.foreground)
                    } else {
                        egui::RichText::new(format!("{schema}  {}", visible.len()))
                            .size(11.0)
                            .color(tk.subtle)
                    };
                    let section = egui::CollapsingHeader::new(header)
                        .id_salt(("schema", schema))
                        .default_open(!needle.is_empty() || is_default || group.len() < AUTO_EXPAND_BELOW)
                        .open(focus.as_ref().map(|f| f == schema))
                        .show(ui, |ui| {
                            for t in visible {
                                let glyph = if t.is_view { icon::EYE } else { icon::TABLE };
                                let is_open = active_table == Some((t.schema.as_str(), t.name.as_str()));
                                let text_color = if is_open {
                                    tk.foreground
                                } else {
                                    tk.muted_foreground
                                };
                                let r = ui.add(
                                    egui::Button::selectable(
                                        is_open,
                                        theme::icon_text(glyph, &t.name, tk.subtle, text_color),
                                    )
                                    .frame_when_inactive(false)
                                    .min_size(egui::vec2(ui.available_width(), 22.0)),
                                );
                                let detail = if by_size {
                                    t.size.map(palette::format_bytes)
                                } else {
                                    t.estimate.map(palette::compact_count)
                                };
                                let r = match (t.estimate, t.size) {
                                    (Some(n), Some(b)) => r.on_hover_text(format!(
                                        "~{} linhas · {}",
                                        palette::format_count(n),
                                        palette::format_bytes(b)
                                    )),
                                    _ => r,
                                };
                                if let Some(count) = detail {
                                    let fits = (t.name.chars().count() + count.len()) as f32 * 7.0 + 40.0
                                        < r.rect.width();
                                    if fits {
                                        ui.painter().text(
                                            r.rect.right_center() - egui::vec2(6.0, 0.0),
                                            egui::Align2::RIGHT_CENTER,
                                            count,
                                            egui::FontId::proportional(11.0),
                                            tk.subtle,
                                        );
                                    }
                                }
                                // One click opens, like a file tree.
                                if r.clicked() {
                                    to_open = Some((t.schema.clone(), t.name.clone()));
                                }
                                r.context_menu(|ui| {
                                    if ui.button("Abrir dados").clicked() {
                                        to_open = Some((t.schema.clone(), t.name.clone()));
                                        ui.close();
                                    }
                                    if ui.button("Estrutura").clicked() {
                                        to_structure = Some((t.schema.clone(), t.name.clone()));
                                        ui.close();
                                    }
                                    if ui.button("SELECT em novo editor").clicked() {
                                        to_sql = Some(format!(
                                            "SELECT * FROM {} LIMIT 100;",
                                            kind.qualified(&t.schema, &t.name)
                                        ));
                                        ui.close();
                                    }
                                    ui.menu_button("Gerar SQL", |ui| {
                                        for (what, label) in [
                                            (crate::sql::Template::Select, "SELECT com as colunas"),
                                            (crate::sql::Template::Insert, "INSERT"),
                                            (crate::sql::Template::Update, "UPDATE pela chave"),
                                            (crate::sql::Template::Delete, "DELETE pela chave"),
                                        ] {
                                            if ui.button(label).clicked() {
                                                to_template = Some((t.schema.clone(), t.name.clone(), what));
                                                ui.close();
                                            }
                                        }
                                    });
                                    if !t.is_view && ui.button("Exportar como .sql…").clicked() {
                                        to_dump = Some(crate::app::DumpScope::Table(
                                            t.schema.clone(),
                                            t.name.clone(),
                                        ));
                                        ui.close();
                                    }
                                    if !t.is_view && !s.cfg.read_only {
                                        ui.separator();
                                        for (action, label) in [
                                            (crate::sql::TableAction::Rename, "Renomear…"),
                                            (crate::sql::TableAction::Duplicate(false), "Duplicar…"),
                                            (crate::sql::TableAction::Truncate, "Esvaziar (TRUNCATE)…"),
                                            (crate::sql::TableAction::Drop, "Excluir tabela…"),
                                        ] {
                                            if ui.button(label).clicked() {
                                                to_action = Some(crate::app::TableActionPrompt {
                                                    schema: t.schema.clone(),
                                                    table: t.name.clone(),
                                                    action,
                                                    input: match action {
                                                        crate::sql::TableAction::Duplicate(_) => {
                                                            format!("{}_copia", t.name)
                                                        }
                                                        crate::sql::TableAction::Rename => t.name.clone(),
                                                        _ => String::new(),
                                                    },
                                                });
                                                ui.close();
                                            }
                                        }
                                    }
                                    if t.is_view && ui.button("Ver código da view").clicked() {
                                        to_object = s
                                            .objects
                                            .iter()
                                            .find(|o| {
                                                o.kind == ObjKind::View
                                                    && o.schema == t.schema
                                                    && o.name == t.name
                                            })
                                            .cloned();
                                        ui.close();
                                    }
                                });
                            }
                            // Functions, procedures, triggers and sequences of this schema (views
                            // are listed with the tables).
                            for kind_ in [
                                ObjKind::Function,
                                ObjKind::Procedure,
                                ObjKind::Trigger,
                                ObjKind::Sequence,
                            ] {
                                let objs: Vec<&DbObject> = s
                                    .objects
                                    .iter()
                                    .filter(|o| o.schema == *schema && o.kind == kind_)
                                    .filter(|o| needle.is_empty() || o.name.to_lowercase().contains(&needle))
                                    .collect();
                                if objs.is_empty() {
                                    continue;
                                }
                                let header =
                                    egui::RichText::new(format!("{}  {}", kind_.plural(), objs.len()))
                                        .size(11.0)
                                        .color(tk.subtle);
                                egui::CollapsingHeader::new(header)
                                    .id_salt(("objs", schema, kind_))
                                    .default_open(!needle.is_empty())
                                    .show(ui, |ui| {
                                        for o in objs {
                                            let glyph = match o.kind {
                                                ObjKind::Function => icon::FUNCTION,
                                                ObjKind::Procedure => icon::GEAR_SIX,
                                                ObjKind::Trigger => icon::LIGHTNING,
                                                ObjKind::Sequence => icon::LIST_NUMBERS,
                                                ObjKind::View => icon::EYE,
                                            };
                                            let r = ui.add(
                                                egui::Button::new(theme::icon_text(
                                                    glyph,
                                                    &o.name,
                                                    tk.subtle,
                                                    tk.muted_foreground,
                                                ))
                                                .frame_when_inactive(false)
                                                .truncate()
                                                .min_size(egui::vec2(ui.available_width(), 22.0)),
                                            );
                                            let tip = match o.kind {
                                                ObjKind::Trigger => {
                                                    format!("{} · tabela {}", o.name, o.table)
                                                }
                                                ObjKind::Function | ObjKind::Procedure => {
                                                    format!("{}({})", o.name, o.args)
                                                }
                                                _ => o.name.clone(),
                                            };
                                            let r = r.on_hover_text(format!("{tip}\nclique abre o código"));
                                            if r.clicked() {
                                                to_object = Some(o.clone());
                                            }
                                            r.context_menu(|ui| {
                                                if matches!(o.kind, ObjKind::Function | ObjKind::Procedure)
                                                    && ui
                                                        .button("Executar…")
                                                        .on_hover_text(
                                                            "Pede os argumentos e mostra o resultado",
                                                        )
                                                        .clicked()
                                                {
                                                    to_run = Some(o.clone());
                                                    ui.close();
                                                }
                                                if ui.button("Ver código").clicked() {
                                                    to_object = Some(o.clone());
                                                    ui.close();
                                                }
                                                if ui.button("Copiar nome").clicked() {
                                                    ui.ctx().copy_text(o.name.clone());
                                                    ui.close();
                                                }
                                            });
                                        }
                                    });
                            }
                        });
                    let section_header = if is_default {
                        section
                            .header_response
                            .on_hover_text("Nomes sem schema no editor (SELECT * FROM tabela) vão para cá")
                    } else {
                        section.header_response
                    };
                    section_header.context_menu(|ui| {
                        if ui.button("Diagrama ER deste schema").clicked() {
                            schema_action = Some((schema.clone(), true));
                            ui.close();
                        }
                        if !s.cfg.read_only && ui.button("Nova tabela aqui").clicked() {
                            schema_action = Some((schema.clone(), false));
                            ui.close();
                        }
                        if ui.button("Exportar schema como .sql…").clicked() {
                            to_dump = Some(crate::app::DumpScope::Schema(schema.clone()));
                            ui.close();
                        }
                        if ui.button("Comparar estrutura com…").clicked() {
                            to_compare = Some(schema.clone());
                            ui.close();
                        }
                    });
                }
            });
        if sort_by_size_toggle {
            self.sort_by_size = !self.sort_by_size;
        }
        if open_activity {
            self.open_activity();
        }
        if new_table {
            self.open_new_table(None);
        }
        // (schema, true): diagram; (schema, false): new table there.
        match schema_action {
            Some((schema, true)) => self.open_diagram(Some(&schema)),
            Some((schema, false)) => self.open_new_table(Some(&schema)),
            None => {}
        }
        if disconnect {
            self.disconnect();
            return;
        }
        if open_sql {
            self.open_sql(String::new());
        }
        if let Some((schema, table)) = to_open {
            self.open_table(&schema, &table);
        }
        if let Some((schema, table)) = to_structure {
            self.open_structure(&schema, &table);
        }
        if let Some(sql) = to_sql {
            self.open_sql(sql);
        }
        if let Some((schema, table, what)) = to_template {
            self.open_template(&schema, &table, what);
        }
        if let Some(o) = to_object {
            self.open_object(o);
        }
        if let Some(o) = to_run {
            self.run_object(&o);
        }
        if let Some(scope) = to_dump {
            self.open_dump(scope);
        }
        if let Some(schema) = to_compare {
            self.open_compare(Some(schema));
        }
        if to_action.is_some() {
            self.table_action = to_action;
        }
    }
}

/// Connection name followed by a small engine tag in its color.
fn conn_label(c: &ConnConfig) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let t = theme::tokens();
    let (tag, color) = match c.kind {
        Kind::Postgres => ("PG", t.info),
        Kind::MySql => ("MySQL", t.warning),
    };
    let mut job = LayoutJob::default();
    if let Some(tag) = c.color {
        let dot = TextFormat::simple(egui::FontId::proportional(11.0), theme::tag_color(tag));
        job.append("● ", 0.0, dot);
    }
    job.append(
        &c.name,
        0.0,
        TextFormat::simple(egui::FontId::proportional(13.0), t.foreground),
    );
    job.append(
        tag,
        8.0,
        TextFormat::simple(egui::FontId::proportional(10.0), color),
    );
    if c.read_only {
        let fmt = TextFormat::simple(egui::FontId::proportional(10.0), t.destructive);
        job.append("somente leitura", 6.0, fmt);
    } else if c.manual_commit {
        let fmt = TextFormat::simple(egui::FontId::proportional(10.0), t.warning);
        job.append("commit manual", 6.0, fmt);
    }
    job
}
