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
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let ssh_ok = form
                        .cfg
                        .ssh
                        .as_ref()
                        .is_none_or(|s| !s.host.trim().is_empty() && !s.user.trim().is_empty());
                    let valid = !form.cfg.name.trim().is_empty() && !duplicate && ssh_ok;
                    save = ui
                        .add_enabled(valid, theme::primary_button(None, "Salvar"))
                        .clicked();
                    cancel = ui.add(theme::outline_button("Cancelar")).clicked();
                });
            });
        cancel |= modal.should_close();
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
                self.notice = Some(Notice {
                    text: format!("Senhas protegidas pelo {}", self.secrets.describe()),
                    error: false,
                });
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
