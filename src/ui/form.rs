use eframe::egui::{self, TextEdit};

use super::icon;
use super::theme;
use crate::app::{App, MasterMode, Notice};
use crate::config;
use crate::db::{ConnConfig, Kind, SshConfig, Tag};

const FIELD_WIDTH: f32 = 280.0;

impl App {
    pub fn connection_form(&mut self, ctx: &egui::Context) {
        let Some(form) = &mut self.form else { return };
        let mut save = false;
        let mut cancel = false;
        let title = if form.editing.is_some() {
            "Editar conexão"
        } else {
            "Nova conexão"
        };
        let t = theme::tokens();
        let duplicate = config::name_taken(&self.saved, &form.cfg.name, form.editing.as_deref());
        // A finished test arrives; a result for other settings is dropped.
        let key = format!("{:?}", form.cfg);
        if let Some(test) = &mut self.conn_test {
            if test.key != key && test.result.is_some() {
                self.conn_test = None;
            } else if let Ok(r) = test.rx.try_recv() {
                test.result = Some(r);
            }
        }
        let test = self.conn_test.as_ref().filter(|t| t.key == key);
        let mut start_test = false;
        // Modal dims everything behind it and closes on Esc / click outside.
        let modal = egui::Modal::new(egui::Id::new("connection_form"))
            .frame(theme::modal_frame(16))
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(egui::RichText::new(title).size(15.0).color(t.foreground));
                ui.label(
                    egui::RichText::new("PostgreSQL ou MySQL")
                        .small()
                        .color(t.muted_foreground),
                );
                ui.add_space(12.0);
                egui::Grid::new("form")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .min_col_width(72.0)
                    .show(ui, |ui| {
                        let c = &mut form.cfg;
                        // Paste a URL from a provider or .env file to fill the fields below.
                        let url_id = egui::Id::new("conn_form_url");
                        let (mut url, url_error) = ui
                            .data(|d| d.get_temp::<(String, Option<String>)>(url_id))
                            .unwrap_or_default();
                        ui.label("URL");
                        ui.horizontal(|ui| {
                            let r = ui.add(
                                TextEdit::singleline(&mut url)
                                    .hint_text("postgres://usuário:senha@host:5432/banco")
                                    .desired_width(FIELD_WIDTH - 80.0),
                            );
                            let fill = ui
                                .add_enabled(!url.trim().is_empty(), egui::Button::new("Preencher"))
                                .clicked()
                                || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                            let error = if fill {
                                match crate::url::apply(c, &url) {
                                    Ok(()) => {
                                        url.clear();
                                        None
                                    }
                                    Err(e) => Some(e),
                                }
                            } else {
                                url_error
                            };
                            ui.data_mut(|d| d.insert_temp(url_id, (url, error)));
                        });
                        ui.end_row();
                        if let Some(e) = ui
                            .data(|d| d.get_temp::<(String, Option<String>)>(url_id))
                            .and_then(|(_, e)| e)
                        {
                            ui.label("");
                            ui.label(egui::RichText::new(e).size(11.0).color(t.destructive));
                            ui.end_row();
                        }
                        ui.label("Nome");
                        ui.add(
                            TextEdit::singleline(&mut c.name)
                                .hint_text("ex: produção")
                                .desired_width(FIELD_WIDTH),
                        );
                        ui.end_row();
                        if duplicate {
                            ui.label("");
                            ui.label(
                                egui::RichText::new("Já existe uma conexão com esse nome")
                                    .size(11.0)
                                    .color(t.destructive),
                            );
                            ui.end_row();
                        }
                        ui.label("Tipo");
                        ui.horizontal(|ui| {
                            for k in [Kind::Postgres, Kind::MySql] {
                                if ui.radio(c.kind == k, k.label()).clicked() && c.kind != k {
                                    if c.port == c.kind.default_port() {
                                        c.port = k.default_port();
                                    }
                                    if c.user == ConnConfig::new(c.kind).user {
                                        c.user = ConnConfig::new(k).user;
                                    }
                                    c.kind = k;
                                }
                            }
                        });
                        ui.end_row();
                        ui.label("Host");
                        ui.add(TextEdit::singleline(&mut c.host).desired_width(FIELD_WIDTH));
                        ui.end_row();
                        ui.label("Porta");
                        ui.add(egui::DragValue::new(&mut c.port).range(1..=65535));
                        ui.end_row();
                        ui.label("Usuário");
                        ui.add(TextEdit::singleline(&mut c.user).desired_width(FIELD_WIDTH));
                        ui.end_row();
                        ui.label("Senha");
                        ui.add(
                            TextEdit::singleline(&mut c.password)
                                .password(true)
                                .desired_width(FIELD_WIDTH),
                        );
                        ui.end_row();
                        ui.label("Database");
                        ui.add(
                            TextEdit::singleline(&mut c.database)
                                .hint_text("opcional")
                                .desired_width(FIELD_WIDTH),
                        );
                        ui.end_row();
                        ui.label("SSL/TLS");
                        ui.checkbox(&mut c.ssl, "Conexão criptografada");
                        ui.end_row();
                        if c.ssl {
                            ui.label("");
                            ui.checkbox(&mut c.ssl_insecure, "Aceitar certificado inválido");
                            ui.end_row();
                        }
                        if !c.ssl && !c.is_local() {
                            ui.label("");
                            let msg = match c.kind {
                                Kind::Postgres => {
                                    "Sem exigir TLS: usa criptografia só se o servidor oferecer"
                                }
                                Kind::MySql => "Sem criptografia: senha e dados trafegam em texto puro",
                            };
                            ui.label(egui::RichText::new(msg).size(11.0).color(theme::tokens().warning));
                            ui.end_row();
                        }
                        ui.label("Proteção");
                        ui.checkbox(&mut c.read_only, "Somente leitura (produção)")
                            .on_hover_text(
                                "Bloqueia edições na grade e deixa a sessão read-only no próprio banco",
                            );
                        ui.end_row();
                        ui.label("");
                        ui.add_enabled(
                            !c.read_only,
                            egui::Checkbox::new(&mut c.manual_commit, "Commit manual"),
                        )
                        .on_hover_text(
                            "Alterações (SQL, grade, importação) ficam numa transação até você clicar em \
                                 Confirmar ou Desfazer",
                        );
                        ui.end_row();
                        ui.label("");
                        ui.add_enabled(
                            !c.read_only,
                            egui::Checkbox::new(&mut c.preview_writes, "Prévia antes de UPDATE/DELETE"),
                        )
                        .on_hover_text(
                            "No editor, mostra quantas e quais linhas seriam alteradas antes de executar",
                        );
                        ui.end_row();
                        ui.label("Cor");
                        color_picker(ui, &mut c.color);
                        ui.end_row();
                        ui.label("Timeout");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut c.timeout_secs)
                                    .range(0..=86_400)
                                    .suffix(" s"),
                            );
                            let hint = if c.timeout_secs == 0 {
                                "sem limite"
                            } else {
                                "por consulta, no servidor"
                            };
                            ui.label(egui::RichText::new(hint).size(11.0).color(t.subtle));
                        });
                        ui.end_row();
                        ui.label("SSH");
                        let mut tunnel = c.ssh.is_some();
                        if ui.checkbox(&mut tunnel, "Conectar por túnel SSH").changed() {
                            c.ssh = tunnel.then(SshConfig::default);
                        }
                        ui.end_row();
                        if let Some(ssh) = &mut c.ssh {
                            ui.label("");
                            ui.horizontal(|ui| {
                                ui.add(
                                    TextEdit::singleline(&mut ssh.user)
                                        .hint_text("usuário")
                                        .desired_width(80.0),
                                );
                                ui.label("@");
                                ui.add(
                                    TextEdit::singleline(&mut ssh.host)
                                        .hint_text("servidor ssh")
                                        .desired_width(124.0),
                                );
                                ui.add(egui::DragValue::new(&mut ssh.port).range(1..=65535));
                            });
                            ui.end_row();
                            ui.label("");
                            ui.add(
                                TextEdit::singleline(&mut ssh.key_path)
                                    .hint_text("chave privada (vazio = agente / chaves padrão)")
                                    .desired_width(FIELD_WIDTH),
                            );
                            ui.end_row();
                            ui.label("");
                            ui.label(
                                egui::RichText::new("Host e porta acima são vistos a partir do servidor SSH")
                                    .size(11.0)
                                    .color(t.subtle),
                            );
                            ui.end_row();
                        }
                    });
                // The test's outcome, while the settings are the ones it ran with (above the
                // buttons, wrapped to the form's width).
                if let Some(test) = &test {
                    let (glyph, text, color) = match &test.result {
                        None => (icon::HOURGLASS, "Conectando…".to_string(), t.muted_foreground),
                        Some(Ok(msg)) => (icon::CHECK_CIRCLE, msg.clone(), t.lime),
                        Some(Err(e)) => (icon::WARNING_CIRCLE, e.clone(), t.destructive),
                    };
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(theme::icon_text(glyph, &format!(" {text}"), color, color)).wrap(),
                    );
                }
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let ssh_ok = form
                        .cfg
                        .ssh
                        .as_ref()
                        .is_none_or(|s| !s.host.trim().is_empty() && !s.user.trim().is_empty());
                    // Say what is missing instead of just greying the button out.
                    let missing = if form.cfg.name.trim().is_empty() {
                        Some("Dê um nome à conexão")
                    } else if duplicate {
                        // Already said next to the name.
                        Some("")
                    } else if !ssh_ok {
                        Some("Preencha usuário e servidor do SSH")
                    } else {
                        None
                    };
                    save = ui
                        .add_enabled(missing.is_none(), theme::primary_button(None, "Salvar"))
                        .on_disabled_hover_text(match missing {
                            Some("") => "Já existe uma conexão com esse nome",
                            m => m.unwrap_or_default(),
                        })
                        .clicked();
                    cancel = ui.add(theme::outline_button("Cancelar")).clicked();
                    let testing = test.as_ref().is_some_and(|t| t.result.is_none());
                    if ui
                        .add_enabled(!testing && ssh_ok, theme::outline_button("Testar conexão"))
                        .on_hover_text("Conecta sem salvar, para conferir host, porta, usuário e senha")
                        .clicked()
                    {
                        start_test = true;
                    }
                    if let Some(m) = missing.filter(|m| !m.is_empty()) {
                        ui.label(egui::RichText::new(m).size(11.0).color(t.warning));
                    }
                });
            });
        cancel |= modal.should_close();
        if start_test && let Some(form) = &self.form {
            self.conn_test = Some(crate::app::ConnTest {
                key: format!("{:?}", form.cfg),
                rx: crate::worker::test_connection(form.cfg.clone(), ctx.clone()),
                result: None,
            });
        }
        if save || cancel {
            self.conn_test = None;
        }
        if save || cancel {
            ctx.data_mut(|d| d.remove::<(String, Option<String>)>(egui::Id::new("conn_form_url")));
        }
        if save {
            if let Some(mut form) = self.form.take() {
                form.cfg.name = form.cfg.name.trim().to_string();
                self.store_connection(form.cfg);
            }
        } else if cancel {
            self.form = None;
        }
    }
}

impl App {
    /// Master password for the encrypted vault: created on first use, asked once per session.
    /// The key derivation runs on a background thread so the window never freezes.
    pub fn master_prompt(&mut self, ctx: &egui::Context) {
        self.finish_master_kdf();
        let Some(p) = &mut self.master else { return };
        let t = theme::tokens();
        let create = p.mode == MasterMode::Create;
        let working = p.working.is_some();
        let (mut submit, mut skip) = (false, false);
        let modal = egui::Modal::new(egui::Id::new("master_password")).frame(theme::modal_frame(16)).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::LOCK_KEY).size(16.0).color(t.lime));
                let title = if create { "Proteger senhas salvas" } else { "Desbloquear senhas" };
                ui.label(egui::RichText::new(title).size(15.0).color(t.foreground));
            });
            ui.add_space(4.0);
            let text = if create {
                "Este sistema não tem um chaveiro (por exemplo, WSL). Crie uma senha mestra: as senhas \
                 das conexões ficam criptografadas em disco e só abrem com ela."
            } else {
                "As senhas das conexões estão criptografadas. Digite a senha mestra para usá-las nesta sessão."
            };
            ui.add(egui::Label::new(egui::RichText::new(text).color(t.muted_foreground)).wrap());
            ui.add_space(12.0);
            ui.add_enabled_ui(!working, |ui| {
                let r = ui.add(
                    TextEdit::singleline(&mut p.password)
                        .password(true)
                        .hint_text("Senha mestra")
                        .desired_width(f32::INFINITY),
                );
                if p.error.is_none() && p.password.is_empty() && !ui.ctx().memory(|m| m.has_focus(r.id)) {
                    r.request_focus();
                }
                let mut enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if create {
                    ui.add_space(4.0);
                    let r2 = ui.add(
                        TextEdit::singleline(&mut p.confirm)
                            .password(true)
                            .hint_text("Repita a senha mestra")
                            .desired_width(f32::INFINITY),
                    );
                    if enter {
                        r2.request_focus();
                        enter = false;
                    }
                    enter |= r2.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                }
                submit = enter;
            });
            if let Some(e) = &p.error {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(e).color(t.destructive));
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Não há como recuperar a senha mestra se ela for esquecida.")
                    .size(11.0)
                    .color(t.subtle),
            );
            ui.add_space(16.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(!working, |ui| {
                    let label = if create { "Criar" } else { "Desbloquear" };
                    submit |= ui.add(theme::primary_button(Some(icon::LOCK_KEY_OPEN), label)).clicked();
                    skip = ui
                        .add(theme::outline_button("Agora não"))
                        .on_hover_text("As senhas ficam só na memória nesta sessão; não são gravadas em disco")
                        .clicked();
                });
                if working {
                    ui.spinner();
                    ui.label(egui::RichText::new("Verificando…").color(t.muted_foreground));
                }
            });
        });
        let skip = !working && (skip || modal.should_close());

        if submit && !working {
            self.start_master_kdf(ctx);
        } else if skip && let Some(p) = self.master.take() {
            self.after_master(p.then);
        }
    }

    fn start_master_kdf(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.master else { return };
        let create = p.mode == MasterMode::Create;
        if create && p.password != p.confirm {
            p.error = Some("As senhas não conferem".into());
            return;
        }
        let Some(vault) = self.secrets.vault_mut() else {
            return;
        };
        match vault.kdf_job(&p.password, create) {
            Ok(job) => {
                let (tx, rx) = std::sync::mpsc::channel();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(job.run());
                    ctx.request_repaint();
                });
                p.error = None;
                p.working = Some(rx);
            }
            Err(e) => p.error = Some(e),
        }
    }

    /// Applies a finished key derivation (or shows why it failed).
    fn finish_master_kdf(&mut self) {
        let Some(p) = &mut self.master else { return };
        let Some(rx) = &p.working else { return };
        let Ok(result) = rx.try_recv() else { return };
        p.working = None;
        let applied = match (result, self.secrets.vault_mut()) {
            (Ok(derived), Some(vault)) => vault.finish(derived),
            (Err(e), _) => Err(e),
            (Ok(_), None) => Ok(()),
        };
        match applied {
            Ok(()) => {
                let then = p.then.clone();
                self.master = None;
                self.notice = Notice::info(format!("Senhas protegidas pelo {}", self.secrets.describe()));
                self.after_master(then);
            }
            Err(e) => {
                p.error = Some(e);
                p.password.clear();
                p.confirm.clear();
            }
        }
    }
}

/// "None" plus one swatch per tag; the selected one gets a ring.
fn color_picker(ui: &mut egui::Ui, color: &mut Option<Tag>) {
    let t = theme::tokens();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let none = egui::RichText::new("nenhuma")
            .size(11.0)
            .color(if color.is_none() { t.foreground } else { t.subtle });
        if ui
            .add(egui::Button::new(none).selected(color.is_none()))
            .clicked()
        {
            *color = None;
        }
        for tag in Tag::ALL {
            let (rect, r) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
            let p = ui.painter();
            p.rect_filled(rect.shrink(2.0), 0.0, theme::tag_color(tag));
            if *color == Some(tag) {
                p.rect_stroke(
                    rect,
                    0.0,
                    egui::Stroke::new(1.5, t.foreground),
                    egui::StrokeKind::Inside,
                );
            }
            if r.on_hover_text(tag.label()).clicked() {
                *color = Some(tag);
            }
        }
    });
}

impl App {
    /// "Salvar consulta": a name, and whether it shows on every connection.
    pub fn favorite_prompt(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.fav_prompt else { return };
        let t = theme::tokens();
        let (mut save, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::new("favorite_prompt"))
            .frame(theme::modal_frame(16))
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.horizontal(|ui| {
                    ui.label(theme::icon(icon::STAR).size(16.0).color(t.warning));
                    ui.label(
                        egui::RichText::new("Salvar consulta")
                            .size(15.0)
                            .color(t.foreground),
                    );
                });
                ui.add_space(8.0);
                let r = ui.add(
                    TextEdit::singleline(&mut p.name)
                        .hint_text("Nome (ex: clientes ativos)")
                        .desired_width(f32::INFINITY),
                );
                if p.name.is_empty() && !r.has_focus() {
                    r.request_focus();
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(4.0);
                ui.checkbox(&mut p.shared, "Mostrar em todas as conexões");
                ui.add_space(4.0);
                egui::Frame::new()
                    .fill(t.sidebar)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let preview: String = p.sql.lines().take(6).collect::<Vec<_>>().join("\n");
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(preview)
                                    .monospace()
                                    .size(12.0)
                                    .color(t.muted_foreground),
                            )
                            .wrap(),
                        );
                    });
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let valid = !p.name.trim().is_empty();
                    save = ui
                        .add_enabled(valid, theme::primary_button(Some(icon::STAR), "Salvar"))
                        .clicked()
                        || (enter && valid);
                    cancel = ui.add(theme::outline_button("Cancelar")).clicked();
                });
            });
        if save {
            if let Some(p) = self.fav_prompt.take() {
                self.add_favorite(p);
            }
        } else if cancel || modal.should_close() {
            self.fav_prompt = None;
        }
    }
}
