use eframe::egui::{self, TextEdit};

use super::icon;
use super::theme;
use crate::app::{App, Dialog, MasterMode, Notice};
use crate::config;
use crate::db::{ConnConfig, Kind, SshConfig, Tag};

const FIELD_WIDTH: f32 = 280.0;

impl App {
    pub fn connection_form(&mut self, ctx: &egui::Context) {
        let Some(form) = &mut self.form else { return };
        let mut save = false;
        let mut cancel = false;
        let title = if form.editing.is_some() {
            "Edit connection"
        } else {
            "New connection"
        };
        let t = theme::tokens();
        let duplicate = config::name_taken(&self.saved, &form.cfg.name, form.editing.as_deref());
        // A finished test arrives; a result for other settings is dropped.
        let key = form.cfg.test_key();
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
                    egui::RichText::new("PostgreSQL, MySQL or SQLite")
                        .small()
                        .color(t.muted_foreground),
                );
                ui.add_space(12.0);
                egui::Grid::new("form")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .min_col_width(72.0)
                    .show(ui, |ui| {
                        let is_new = form.editing.is_none();
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
                                    .hint_text("postgres://user:password@host:5432/database")
                                    .desired_width(FIELD_WIDTH - 80.0),
                            );
                            let fill = ui
                                .add_enabled(!url.trim().is_empty(), egui::Button::new("Fill in"))
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
                        ui.label("Name");
                        ui.add(
                            TextEdit::singleline(&mut c.name)
                                .hint_text("e.g. production")
                                .desired_width(FIELD_WIDTH),
                        );
                        ui.end_row();
                        if duplicate {
                            ui.label("");
                            ui.label(
                                egui::RichText::new("A connection with this name already exists")
                                    .size(11.0)
                                    .color(t.destructive),
                            );
                            ui.end_row();
                        }
                        ui.label("Type");
                        ui.horizontal(|ui| {
                            for k in [Kind::Postgres, Kind::MySql, Kind::Sqlite] {
                                if ui.radio(c.kind == k, k.label()).clicked() && c.kind != k {
                                    if k == Kind::Sqlite {
                                        // A file: nothing to tunnel or encrypt.
                                        c.ssh = None;
                                        c.ssl = false;
                                        c.timeout_secs = 0;
                                    }
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
                        if c.kind == Kind::Sqlite {
                            sqlite_file_rows(ui, c);
                        } else {
                            ui.label("Host");
                            let was_local = c.is_local();
                            let host = ui.add(TextEdit::singleline(&mut c.host).desired_width(FIELD_WIDTH));
                            // A new connection to another machine starts encrypted (and verified),
                            // unless the user already chose otherwise for the previous host. Through
                            // SSH the tunnel already encrypts.
                            if host.changed() && is_new && c.ssh.is_none() && c.ssl != was_local {
                                c.ssl = !c.is_local();
                            }
                            ui.end_row();
                            ui.label("Port");
                            ui.add(egui::DragValue::new(&mut c.port).range(1..=65535));
                            ui.end_row();
                            ui.label("User");
                            ui.add(TextEdit::singleline(&mut c.user).desired_width(FIELD_WIDTH));
                            ui.end_row();
                            ui.label("Password");
                            ui.add(
                                TextEdit::singleline(&mut c.password)
                                    .password(true)
                                    .desired_width(FIELD_WIDTH),
                            );
                            ui.end_row();
                            ui.label("Database");
                            ui.add(
                                TextEdit::singleline(&mut c.database)
                                    .hint_text("optional")
                                    .desired_width(FIELD_WIDTH),
                            );
                            ui.end_row();
                            ui.label("SSL/TLS");
                            ui.checkbox(&mut c.ssl, "Encrypted connection");
                            ui.end_row();
                            if c.ssl {
                                ui.label("");
                                ui.checkbox(&mut c.ssl_insecure, "Accept invalid certificate");
                                ui.end_row();
                            }
                            // Through SSH the tunnel encrypts the way out.
                            if !c.ssl && !c.is_local() && c.ssh.is_none() {
                                ui.label("");
                                let msg = match c.kind {
                                    Kind::Postgres => "No TLS: the password can be intercepted on the network",
                                    Kind::MySql => "No encryption: password and data travel in plain text",
                                    Kind::Sqlite => "",
                                };
                                ui.label(egui::RichText::new(msg).size(11.0).color(theme::tokens().warning));
                                ui.end_row();
                            }
                        }
                        ui.label("Protection");
                        ui.checkbox(&mut c.read_only, "Read-only (production)")
                            .on_hover_text(
                                "Blocks edits in the grid and makes the session read-only in the database itself",
                            );
                        ui.end_row();
                        ui.label("");
                        ui.add_enabled(
                            !c.read_only,
                            egui::Checkbox::new(&mut c.manual_commit, "Manual commit"),
                        )
                        .on_hover_text(
                            "Changes (SQL, grid, import) stay in a transaction until you click \
                                 Commit or Roll back",
                        );
                        ui.end_row();
                        ui.label("");
                        ui.add_enabled(
                            !c.read_only,
                            egui::Checkbox::new(&mut c.preview_writes, "Preview before UPDATE/DELETE"),
                        )
                        .on_hover_text(
                            "In the editor, shows how many and which rows would change before running",
                        );
                        ui.end_row();
                        ui.label("Color");
                        color_picker(ui, &mut c.color);
                        ui.end_row();
                        if c.kind != Kind::Sqlite {
                            ui.label("Timeout");
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::DragValue::new(&mut c.timeout_secs)
                                        .range(0..=86_400)
                                        .suffix(" s"),
                                );
                                let hint = if c.timeout_secs == 0 {
                                    "no limit"
                                } else {
                                    "per query, on the server"
                                };
                                ui.label(egui::RichText::new(hint).size(11.0).color(t.subtle));
                            });
                            ui.end_row();
                            ui.label("SSH");
                            let mut tunnel = c.ssh.is_some();
                            if ui.checkbox(&mut tunnel, "Connect through an SSH tunnel").changed() {
                                c.ssh = tunnel.then(SshConfig::default);
                            }
                            ui.end_row();
                            if let Some(ssh) = &mut c.ssh {
                                ui.label("");
                                ui.horizontal(|ui| {
                                    ui.add(
                                        TextEdit::singleline(&mut ssh.user)
                                            .hint_text("user")
                                            .desired_width(80.0),
                                    );
                                    ui.label("@");
                                    ui.add(
                                        TextEdit::singleline(&mut ssh.host)
                                            .hint_text("ssh server")
                                            .desired_width(124.0),
                                    );
                                    ui.add(egui::DragValue::new(&mut ssh.port).range(1..=65535));
                                });
                                ui.end_row();
                                ui.label("");
                                ui.add(
                                    TextEdit::singleline(&mut ssh.key_path)
                                        .hint_text("private key (empty = agent / default keys)")
                                        .desired_width(FIELD_WIDTH),
                                );
                                ui.end_row();
                                ui.label("");
                                ui.label(
                                    egui::RichText::new(
                                        "Host and port above are seen from the SSH server",
                                    )
                                    .size(11.0)
                                    .color(t.subtle),
                                );
                                ui.end_row();
                            }
                        }
                    });
                // The test's outcome, while the settings are the ones it ran with (above the
                // buttons, wrapped to the form's width).
                if let Some(test) = &test {
                    let (glyph, text, color) = match &test.result {
                        None => (icon::HOURGLASS, "Connecting…".to_string(), t.muted_foreground),
                        Some(Ok(msg)) => (icon::CHECK_CIRCLE, msg.clone(), t.lime),
                        Some(Err(e)) => (icon::WARNING_CIRCLE, e.clone(), t.destructive),
                    };
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(theme::icon_text(glyph, &format!(" {text}"), color, color)).wrap(),
                    );
                }
                ui.add_space(16.0);
                // One row tall: a right-to-left layout would otherwise take every bit of height left,
                // and the modal would keep the height of a longer form (e.g. after switching to SQLite).
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 28.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let ssh_ok = form
                            .cfg
                            .ssh
                            .as_ref()
                            .is_none_or(|s| !s.host.trim().is_empty() && !s.user.trim().is_empty());
                        // Say what is missing instead of just greying the button out.
                        let missing = if form.cfg.name.trim().is_empty() {
                            Some("Give the connection a name")
                        } else if form.cfg.kind == Kind::Sqlite && form.cfg.database.trim().is_empty() {
                            Some("Choose the database file")
                        } else if duplicate {
                            // Already said next to the name.
                            Some("")
                        } else if !ssh_ok {
                            Some("Fill in the SSH user and server")
                        } else {
                            None
                        };
                        save = ui
                            .add_enabled(missing.is_none(), theme::primary_button(None, "Save"))
                            .on_disabled_hover_text(match missing {
                                Some("") => "A connection with this name already exists",
                                m => m.unwrap_or_default(),
                            })
                            .clicked();
                        cancel = ui.add(theme::outline_button("Cancel")).clicked();
                        let testing = test.as_ref().is_some_and(|t| t.result.is_none());
                        if ui
                            .add_enabled(!testing && ssh_ok, theme::outline_button("Test connection"))
                            .on_hover_text("Connects without saving, to check host, port, user and password")
                            .clicked()
                        {
                            start_test = true;
                        }
                        if let Some(m) = missing.filter(|m| !m.is_empty()) {
                            ui.label(egui::RichText::new(m).size(11.0).color(t.warning));
                        }
                    },
                );
            });
        cancel |= modal.should_close();
        if start_test && let Some(form) = &self.form {
            self.conn_test = Some(crate::app::ConnTest {
                key: form.cfg.test_key(),
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
                let title = if create { "Protect saved passwords" } else { "Unlock passwords" };
                ui.label(egui::RichText::new(title).size(15.0).color(t.foreground));
            });
            ui.add_space(4.0);
            let text = if create {
                "This system has no keyring (WSL, for example). Create a master password: the connection \
                 passwords are encrypted on disk and only open with it."
            } else {
                "The connection passwords are encrypted. Type the master password to use them in this session."
            };
            ui.add(egui::Label::new(egui::RichText::new(text).color(t.muted_foreground)).wrap());
            ui.add_space(12.0);
            ui.add_enabled_ui(!working, |ui| {
                let r = ui.add(
                    TextEdit::singleline(&mut p.password)
                        .password(true)
                        .hint_text("Master password")
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
                            .hint_text("Repeat the master password")
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
                egui::RichText::new("There's no way to recover the master password if you forget it.")
                    .size(11.0)
                    .color(t.subtle),
            );
            ui.add_space(16.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(!working, |ui| {
                    let label = if create { "Create" } else { "Unlock" };
                    submit |= ui.add(theme::primary_button(Some(icon::LOCK_KEY_OPEN), label)).clicked();
                    skip = ui
                        .add(theme::outline_button("Not now"))
                        .on_hover_text("Passwords stay in memory only for this session; they aren't written to disk")
                        .clicked();
                });
                if working {
                    ui.spinner();
                    ui.label(egui::RichText::new("Checking…").color(t.muted_foreground));
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
            p.error = Some("The passwords don't match".into());
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
                self.notice = Notice::info(format!("Passwords protected by the {}", self.secrets.describe()));
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
        let none = egui::RichText::new("none").size(11.0).color(if color.is_none() {
            t.foreground
        } else {
            t.subtle
        });
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
    /// "Save query": a name, and whether it shows on every connection.
    pub fn favorite_prompt(&mut self, ctx: &egui::Context) {
        let Some(Dialog::Favorite(p)) = &mut self.dialog else {
            return;
        };
        let t = theme::tokens();
        let (mut save, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::new("favorite_prompt"))
            .frame(theme::modal_frame(16))
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.horizontal(|ui| {
                    ui.label(theme::icon(icon::STAR).size(16.0).color(t.warning));
                    ui.label(egui::RichText::new("Save query").size(15.0).color(t.foreground));
                });
                ui.add_space(8.0);
                let r = ui.add(
                    TextEdit::singleline(&mut p.name)
                        .hint_text("Name (e.g. active customers)")
                        .desired_width(f32::INFINITY),
                );
                if p.name.is_empty() && !r.has_focus() {
                    r.request_focus();
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(4.0);
                ui.checkbox(&mut p.shared, "Show on every connection");
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
                        .add_enabled(valid, theme::primary_button(Some(icon::STAR), "Save"))
                        .clicked()
                        || (enter && valid);
                    cancel = ui.add(theme::outline_button("Cancel")).clicked();
                });
            });
        if save {
            if let Some(Dialog::Favorite(p)) = self.dialog.take() {
                self.add_favorite(p);
            }
        } else if cancel || modal.should_close() {
            self.dialog = None;
        }
    }
}

/// SQLite: the database file, typed, picked or created here.
fn sqlite_file_rows(ui: &mut egui::Ui, c: &mut ConnConfig) {
    let t = theme::tokens();
    let msg_id = egui::Id::new("sqlite_file_msg");
    ui.label("File");
    ui.add(
        TextEdit::singleline(&mut c.database)
            .hint_text(r"C:\data\shop.db")
            .desired_width(FIELD_WIDTH),
    );
    ui.end_row();
    ui.label("");
    let mut msg: Option<(bool, String)> = None;
    ui.horizontal(|ui| {
        let mut chosen: Option<std::path::PathBuf> = None;
        if crate::files::NATIVE_DIALOGS && ui.button("Escolher…").clicked() {
            chosen = crate::files::pick_database();
        }
        let create = ui
            .button("Create new file…")
            .on_hover_text("An empty SQLite database, in the chosen file (or at the path typed above)")
            .clicked();
        if create {
            let path = if crate::files::NATIVE_DIALOGS {
                crate::files::pick_new_database()
            } else {
                Some(std::path::PathBuf::from(c.database.trim())).filter(|p| !p.as_os_str().is_empty())
            };
            if let Some(p) = path {
                match crate::sqlite::create(&p.display().to_string()) {
                    Ok(()) => {
                        msg = Some((false, "File created".into()));
                        chosen = Some(p);
                    }
                    Err(e) => msg = Some((true, e.to_string())),
                }
            }
        }
        if let Some(p) = chosen {
            if c.name.trim().is_empty() {
                c.name = p
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
            c.database = p.display().to_string();
        }
    });
    ui.end_row();
    if let Some(m) = msg {
        ui.data_mut(|d| d.insert_temp(msg_id, m));
    }
    if let Some((error, text)) = ui.data(|d| d.get_temp::<(bool, String)>(msg_id)) {
        ui.label("");
        let color = if error { t.destructive } else { t.lime };
        ui.label(egui::RichText::new(text).size(11.0).color(color));
        ui.end_row();
    }
}
