use eframe::egui::{self, TextEdit};

use super::{icon, palette, theme};
use crate::app::{App, ConnForm, TabKind};
use crate::db::{ConnConfig, Kind, TableRef};
use crate::worker::Job;

/// Schemas with fewer tables than this start expanded.
const AUTO_EXPAND_BELOW: usize = 200;

impl App {
    pub fn side_panel(&mut self, ui: &mut egui::Ui) {
        self.connection_list(ui);
        ui.separator();
        self.table_tree(ui);
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
            TabKind::Sql { .. }
            | TabKind::Activity(_)
            | TabKind::Design(_)
            | TabKind::Search(_)
            | TabKind::Diagram(_) => None,
        });
        let needle = self.filter.to_lowercase();
        let by_size = self.sort_by_size;
        let kind = s.cfg.kind;
        let mut to_open: Option<(String, String)> = None;
        let mut to_sql: Option<String> = None;
        let mut to_structure: Option<(String, String)> = None;
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.spacing_mut().button_padding = egui::vec2(6.0, 2.0);
                for group in s.tables.chunk_by(|a, b| a.schema == b.schema) {
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
                    let header = egui::RichText::new(format!("{schema}  {}", visible.len()))
                        .size(11.0)
                        .color(tk.subtle);
                    let section = egui::CollapsingHeader::new(header)
                        .id_salt(("schema", schema))
                        .default_open(!needle.is_empty() || group.len() < AUTO_EXPAND_BELOW)
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
                                });
                            }
                        });
                    section.header_response.context_menu(|ui| {
                        if ui.button("Diagrama ER deste schema").clicked() {
                            schema_action = Some((schema.clone(), true));
                            ui.close();
                        }
                        if !s.cfg.read_only && ui.button("Nova tabela aqui").clicked() {
                            schema_action = Some((schema.clone(), false));
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
