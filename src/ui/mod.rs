mod chart;
mod complete;
mod design;
mod detail;
mod diagram;
mod find;
mod form;
mod grid;
mod help;
mod import_form;
mod palette;
mod paragraphs;
#[cfg(test)]
mod perf;
mod plan_view;
mod prefs;
mod sidebar;
mod structure;
mod syntax;
mod tabs;
pub mod theme;
mod toast;
mod welcome;

use eframe::egui;

use crate::app::{App, Dialog};

/// The app's window is egui's.
impl crate::app::Window for egui::Context {
    fn wake(&self) {
        self.request_repaint();
    }

    fn close(&self) {
        self.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn set_title(&self, title: &str) {
        self.send_viewport_cmd(egui::ViewportCommand::Title(title.into()));
    }

    fn request_attention(&self) {
        self.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Informational,
        ));
    }

    fn focused(&self) -> Option<bool> {
        self.input(|i| i.viewport().focused)
    }
}

pub use egui_phosphor::regular as icon;

impl App {
    /// Small FPS readout in the bottom-left corner (under the sidebar); click it to toggle max-rate redraw.
    fn fps_overlay(&mut self, ctx: &egui::Context) {
        if !self.fps.show {
            return;
        }
        let t = theme::tokens();
        egui::Area::new(egui::Id::new("fps"))
            .anchor(egui::Align2::LEFT_BOTTOM, [8.0, -4.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                // Off by default: frames are only drawn when something changes, so the count
                // means little until max-rate redraw is switched on.
                let (label, color) = if self.fps.max {
                    (format!("{:.0} FPS", self.fps.value), t.warning)
                } else {
                    ("FPS".to_string(), t.subtle)
                };
                let text = egui::RichText::new(label).small().color(color);
                let r = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                if r.clicked() {
                    self.fps.max = !self.fps.max;
                }
                r.on_hover_text(if self.fps.max {
                    "Repainting at the monitor's rate. Click to save CPU."
                } else {
                    "Repainting on demand. Click to use the maximum rate."
                });
            });
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        self.poll_backup();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Nowhere left to show it: the window is closing.
        if let Err(e) = crate::session::save(&self.snapshot()) {
            eprintln!("tabella: couldn't save the session: {e}");
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

impl App {
    /// One frame of the whole window.
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        self.tick_fps();
        self.forget_closed_tabs(ui.ctx());
        self.clear_finished_notice();
        // Not while minimized: the window may say 0 x 0 then, and that would be saved.
        if ui.ctx().input(|i| i.viewport().minimized) != Some(true) {
            self.window_state.size = ui
                .ctx()
                .input(|i| i.viewport().inner_rect)
                .map(|r| [r.width(), r.height()]);
        }
        if self.fps.max {
            // vsync paces this to the monitor's refresh rate.
            ui.ctx().request_repaint();
        }
        self.shortcuts(ui.ctx());
        let t = theme::tokens();
        // Environment marker: a colored band across the window while connected to a tagged
        // connection, so production is never mistaken for staging.
        if let Some(tag) = self.session.as_ref().and_then(|s| s.cfg.color) {
            egui::Panel::top("env_band")
                .exact_size(3.0)
                .frame(egui::Frame::new().fill(theme::tag_color(tag)))
                .show_separator_line(false)
                .show(ui, |_| {});
        }
        self.tx_band(ui);
        self.quit_guard(ui.ctx());
        egui::Panel::left("side")
            .frame(
                egui::Frame::new()
                    .fill(t.sidebar)
                    .inner_margin(egui::Margin::same(8)),
            )
            .default_size(232.0)
            .size_range(184.0..=360.0)
            .show(ui, |ui| self.side_panel(ui));
        // No margin: the tab strip, toolbars and grid run edge to edge, like a data tool.
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(t.background))
            .show(ui, |ui| self.central(ui));
        self.fps_overlay(ui.ctx());
        self.notice_toast(ui.ctx());
        let ctx = ui.ctx().clone();
        self.connection_form(&ctx);
        self.palette_ui(&ctx);
        self.master_prompt(&ctx);
        self.favorite_prompt(&ctx);
        self.help_ui(&ctx);
        self.path_prompt_ui(&ctx);
        self.table_action_ui(&ctx);
        self.track_zoom(&ctx);
        self.follow_system_theme(&ctx);
    }
}

impl App {
    /// Drops the UI caches (laid-out text, search hits, comparisons) of tabs closed since the
    /// last frame, whichever way they were closed: they live in egui's memory, keyed by the tab,
    /// and would otherwise stay until the app quits (a big script's layout is hundreds of MB).
    fn forget_closed_tabs(&mut self, ctx: &egui::Context) {
        if self.tabs.len() == self.open_tab_ids.len()
            && self
                .tabs
                .iter()
                .zip(&self.open_tab_ids)
                .all(|(t, id)| t.id == *id)
        {
            return;
        }
        for &id in &self.open_tab_ids {
            if !self.tabs.iter().any(|t| t.id == id) {
                tabs::forget_tab_caches(ctx, id);
            }
        }
        self.open_tab_ids = self.tabs.iter().map(|t| t.id).collect();
    }

    /// "Open transaction" across the window while a manual transaction has uncommitted work.
    fn tx_band(&mut self, ui: &mut egui::Ui) {
        let Some(txn) = self.session.as_ref().map(|s| s.txn).filter(|t| t.open) else {
            return;
        };
        let cells = self.session.as_ref().map_or(0, |s| s.uncommitted.len());
        let t = theme::tokens();
        let accent = if txn.failed { t.destructive } else { t.warning };
        let mut end = None;
        egui::Panel::top("tx_band")
            .frame(
                egui::Frame::new()
                    .fill(theme::mix(accent, 0.16, t.background))
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(theme::icon(icon::WARNING).color(accent));
                    let text = if txn.failed {
                        "Open transaction with an error: a statement failed, it can only be rolled back"
                            .to_string()
                    } else if txn.writes == 0 {
                        "Open transaction: nothing was written yet".to_string()
                    } else {
                        format!(
                            "Open transaction · {} write statement(s) not committed yet",
                            txn.writes
                        )
                    };
                    ui.label(egui::RichText::new(text).color(t.foreground));
                    if cells > 0 {
                        // The same tint the grid gives those cells.
                        let swatch = egui::RichText::new("  ").background_color(theme::mix(
                            t.lime,
                            0.35,
                            t.background,
                        ));
                        ui.label(swatch);
                        ui.label(
                            egui::RichText::new(format!("{cells} edited cell(s) marked in the grids"))
                                .color(t.muted_foreground),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!txn.failed, theme::primary_button(Some(icon::CHECK), "Commit"))
                            .on_hover_text("COMMIT: writes everything")
                            .clicked()
                        {
                            end = Some(true);
                        }
                        if ui
                            .add(theme::outline_button("Roll back"))
                            .on_hover_text("ROLLBACK: discards everything since the transaction started")
                            .clicked()
                        {
                            end = Some(false);
                        }
                    });
                });
            });
        if let Some(commit) = end {
            self.end_tx(commit);
        }
    }

    /// Rename / duplicate / empty / drop a table. Emptying and dropping ask for the table's
    /// name typed in full before the (red) button works.
    fn table_action_ui(&mut self, ctx: &egui::Context) {
        use crate::sql::TableAction;
        let Some(Dialog::TableAction(p)) = &mut self.dialog else {
            return;
        };
        let Some(s) = &self.session else {
            self.dialog = None;
            return;
        };
        let t = theme::tokens();
        let rows = s
            .tables
            .iter()
            .find(|x| x.schema == p.schema && x.name == p.table)
            .and_then(|x| x.estimate)
            .map(|n| format!("~{} rows", palette::format_count(n)))
            .unwrap_or_else(|| "rows".into());
        let destructive = matches!(p.action, TableAction::Truncate | TableAction::Drop);
        let (title, verb) = match p.action {
            TableAction::Rename => ("Rename table", "Rename"),
            TableAction::Duplicate(_) => ("Duplicate table", "Duplicate"),
            TableAction::Truncate => ("Truncate table", "Truncate"),
            TableAction::Drop => ("Drop table", "Drop"),
        };
        let ready = match p.action {
            TableAction::Rename | TableAction::Duplicate(_) => {
                !p.input.trim().is_empty() && p.input.trim() != p.table
            }
            TableAction::Truncate | TableAction::Drop => p.input.trim() == p.table,
        };
        let sql = crate::sql::table_action(s.cfg.kind, &p.schema, &p.table, p.action, &p.input);
        let (mut go, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::new("table_action"))
            .frame(theme::modal_frame(16))
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.horizontal(|ui| {
                    let (glyph, color) = if destructive { (icon::WARNING, t.destructive) } else { (icon::TABLE, t.lime) };
                    ui.label(theme::icon(glyph).size(16.0).color(color));
                    ui.label(egui::RichText::new(title).size(15.0).color(t.foreground));
                    ui.label(egui::RichText::new(format!("{}.{}", p.schema, p.table)).monospace().color(t.muted_foreground));
                });
                ui.add_space(8.0);
                let hint = match p.action {
                    TableAction::Rename => "New name:".to_string(),
                    TableAction::Duplicate(_) => "Name of the copy (same columns, indexes and defaults):".to_string(),
                    TableAction::Truncate => format!(
                        "Deletes all {rows} of {}. The structure stays. It can't be undone (except in manual commit).",
                        p.table
                    ),
                    TableAction::Drop => format!(
                        "Deletes all of {}: structure, indexes and all {rows}. It can't be undone (except in manual commit).",
                        p.table
                    ),
                };
                let hint_color = if destructive { t.destructive } else { t.muted_foreground };
                ui.add(egui::Label::new(egui::RichText::new(hint).color(hint_color)).wrap());
                if destructive {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(format!("Type {} to confirm:", p.table)).size(12.0).color(t.subtle));
                }
                let r = ui.add(
                    egui::TextEdit::singleline(&mut p.input)
                        .font(egui::FontId::monospace(12.5))
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
                if let TableAction::Duplicate(with_data) = &mut p.action {
                    ui.checkbox(with_data, format!("copy the rows too ({rows})"));
                }
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(t.sidebar)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let job = syntax::highlight(&sql, &egui::FontId::monospace(12.0));
                        ui.add(egui::Label::new(job).wrap());
                    });
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button = if destructive {
                        egui::Button::new(egui::RichText::new(verb).color(t.foreground)).fill(t.destructive)
                    } else {
                        theme::primary_button(None, verb)
                    };
                    if ui.add_enabled(ready, button).clicked() {
                        go = true;
                    }
                    if ui.add(theme::outline_button("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if go {
            if let Some(Dialog::TableAction(p)) = self.dialog.take() {
                self.run_table_action(&p);
            }
        } else if cancel || modal.should_close() {
            self.dialog = None;
        }
    }

    /// Where there's no native file dialog: type the path.
    fn path_prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(Dialog::Path(p)) = &mut self.dialog else {
            return;
        };
        let t = theme::tokens();
        let (mut ok, mut cancel) = (false, false);
        let saving = p.save_tab.is_some()
            || matches!(
                p.purpose,
                Some(crate::app::PathUse::ExportConnections | crate::app::PathUse::Backup)
            );
        let note = match p.purpose {
            Some(crate::app::PathUse::Backup) => Some(
                "Uses pg_dump / mysqldump: a .sql name is plain text; for PostgreSQL any other name is the compressed format.",
            ),
            Some(crate::app::PathUse::Restore) => Some(
                "Runs the file against this database with psql / pg_restore / mysql. Objects that already exist cause errors.",
            ),
            _ => None,
        };
        let modal = egui::Modal::new(egui::Id::new("path_prompt"))
            .frame(theme::modal_frame(16))
            .show(ctx, |ui| {
                ui.set_width(480.0);
                ui.label(
                    egui::RichText::new(match (p.purpose, saving) {
                        (Some(crate::app::PathUse::Backup), _) => "Back up database",
                        (Some(crate::app::PathUse::Restore), _) => "Restore database",
                        (Some(_), true) => "Export connections",
                        (Some(_), false) => "Import connections",
                        (None, true) => "Save as",
                        (None, false) => "Open SQL file",
                    })
                    .size(15.0)
                    .color(t.foreground),
                );
                if let Some(note) = note {
                    ui.label(egui::RichText::new(note).size(11.0).color(t.subtle));
                }
                ui.add_space(8.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut p.path)
                        .hint_text(if p.purpose.is_some() {
                            "/path/to/connections.json"
                        } else {
                            "/path/to/file.sql"
                        })
                        .font(egui::FontId::monospace(12.5))
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    ok = true;
                }
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(theme::primary_button(None, if saving { "Save" } else { "Open" }))
                        .clicked()
                    {
                        ok = true;
                    }
                    if ui.add(theme::outline_button("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if ok
            && !p.path.trim().is_empty()
            && let Some(Dialog::Path(p)) = self.dialog.take()
        {
            let path = std::path::PathBuf::from(p.path.trim());
            match p.purpose {
                Some(crate::app::PathUse::ExportConnections) => return self.export_connections(&path),
                Some(crate::app::PathUse::ImportConnections) => return self.import_connections(&path),
                Some(crate::app::PathUse::Backup) => return self.start_backup(&path, false),
                Some(crate::app::PathUse::Restore) => return self.start_backup(&path, true),
                None => {}
            }
            match p
                .save_tab
                .and_then(|id| self.tabs.iter().position(|t| t.id == id))
            {
                Some(i) => self.save_file_as(i, &path),
                None if p.run => self.open_script(&path),
                None => self.open_file(path),
            }
        } else if cancel || modal.should_close() {
            self.dialog = None;
        }
    }

    /// Closing the window would silently drop unsaved grid edits (the session doesn't keep them)
    /// or roll back an open transaction: ask first, about the edits and then the transaction.
    fn quit_guard(&mut self, ctx: &egui::Context) {
        let edited: Vec<&str> = if self.window_state.quit_drops_edits {
            Vec::new()
        } else {
            self.tabs
                .iter()
                .filter(|t| crate::app::has_unsaved(t))
                .map(|t| t.title.as_str())
                .collect()
        };
        let open = self.session.as_ref().is_some_and(|s| s.txn.open) && !self.window_state.quit_after_tx;
        if (open || !edited.is_empty()) && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.window_state.quit_prompt = true;
        }
        if !self.window_state.quit_prompt {
            return;
        }
        if !edited.is_empty() {
            let edited = edited.join(", ");
            self.unsaved_edits_prompt(ctx, &edited);
            return;
        }
        let t = theme::tokens();
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("quit_tx"))
            .frame(theme::modal_frame(16))
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.horizontal(|ui| {
                    ui.label(theme::icon(icon::WARNING).size(16.0).color(t.warning));
                    ui.label(
                        egui::RichText::new("Open transaction")
                            .size(15.0)
                            .color(t.foreground),
                    );
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "There are uncommitted changes. Closing without deciding rolls them all back.",
                    )
                    .color(t.muted_foreground),
                );
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(theme::primary_button(Some(icon::CHECK), "Commit and quit"))
                        .clicked()
                    {
                        choice = Some(Some(true));
                    }
                    if ui.add(theme::outline_button("Roll back and quit")).clicked() {
                        choice = Some(Some(false));
                    }
                    if ui.add(theme::outline_button("Back")).clicked() {
                        choice = Some(None);
                    }
                });
            });
        if modal.should_close() && choice.is_none() {
            choice = Some(None);
        }
        match choice {
            Some(Some(commit)) => {
                self.window_state.quit_prompt = false;
                self.window_state.quit_after_tx = true;
                self.end_tx(commit);
            }
            Some(None) => self.window_state.quit_prompt = false,
            None => {}
        }
    }

    /// "Unsaved changes" before quitting: back to save them, or quit without them.
    fn unsaved_edits_prompt(&mut self, ctx: &egui::Context, tabs: &str) {
        let t = theme::tokens();
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("quit_edits"))
            .frame(theme::modal_frame(16))
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.horizontal(|ui| {
                    ui.label(theme::icon(icon::WARNING).size(16.0).color(t.warning));
                    ui.label(egui::RichText::new("Unsaved changes").size(15.0).color(t.foreground));
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Edits in {tabs} aren't saved yet. Closing now discards them; go back and press Ctrl+S to keep them."
                    ))
                    .color(t.muted_foreground),
                );
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(tabs::danger_button("Discard and quit")).clicked() {
                        choice = Some(true);
                    }
                    if ui.add(theme::outline_button("Back")).clicked() {
                        choice = Some(false);
                    }
                });
            });
        if modal.should_close() && choice.is_none() {
            choice = Some(false);
        }
        match choice {
            Some(true) => {
                self.window_state.quit_drops_edits = true;
                self.window_state.quit_prompt = false;
                // Asked again: a transaction still open gets its own question.
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(false) => self.window_state.quit_prompt = false,
            None => {}
        }
    }

    /// App-wide keys. Consumed here so text fields don't also react to them.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F1)) {
            self.dialog = match self.dialog {
                Some(Dialog::Help) => None,
                _ => Some(Dialog::Help),
            };
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::F))
            && self.session.as_ref().is_some_and(|s| s.ready)
        {
            self.open_search();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::H))
            && self.session.as_ref().is_some_and(|s| s.ready)
        {
            self.open_history();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O))
            && self.session.as_ref().is_some_and(|s| s.ready)
        {
            self.open_file_dialog();
        }
        // A .sql file dropped on the window opens in an editor tab.
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("sql")))
                .collect()
        });
        if self.session.as_ref().is_some_and(|s| s.ready) {
            for p in dropped {
                self.open_file(p);
            }
        }
        // Before Ctrl+T, which would match it too.
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::T)) {
            self.reopen_closed_tab();
        }
        let (palette, new_sql, close, prev, next, split) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::P) || i.consume_key(Modifiers::COMMAND, Key::K),
                i.consume_key(Modifiers::COMMAND, Key::T),
                i.consume_key(Modifiers::COMMAND, Key::W),
                // Shift variant first: the plain one would also match Ctrl+Shift+Tab.
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Tab),
                i.consume_key(Modifiers::COMMAND, Key::Tab),
                i.consume_key(Modifiers::COMMAND, Key::Backslash),
            )
        });
        if palette {
            self.dialog = match self.dialog {
                Some(Dialog::Palette(_)) => None,
                _ => Some(Dialog::Palette(Default::default())),
            };
        }
        let ready = self.session.as_ref().is_some_and(|s| s.ready);
        if new_sql && ready {
            self.open_sql(String::new());
        }
        if close {
            if self.tabs.get(self.active).is_some_and(|t| t.pinned) {
                self.notice =
                    crate::app::Notice::info("This tab is pinned: unpin it (right-click) to close it");
            } else {
                self.close_tab(self.active);
            }
        }
        // In a split view Ctrl+Tab cycles the tabs of the focused pane.
        let right = self.tabs.get(self.active).is_some_and(|t| t.right);
        let pane: Vec<usize> = self.pane_tabs(right).collect();
        let n = pane.len();
        if let Some(pos) = pane.iter().position(|&i| i == self.active) {
            if next {
                self.active = pane[(pos + 1) % n];
            }
            if prev {
                self.active = pane[(pos + n - 1) % n];
            }
        }
        if split && ready && !self.tabs.is_empty() {
            self.split_tab(self.active);
        }
        // Alt+Left (or the mouse's back button) undoes "follow foreign key". Not while typing:
        // text fields use Alt+Left themselves.
        let typing = ctx.egui_wants_keyboard_input();
        let back = ctx.input_mut(|i| {
            let key = !typing && i.consume_key(Modifiers::ALT, Key::ArrowLeft);
            key || i.pointer.button_pressed(egui::PointerButton::Extra1)
        });
        if back {
            self.go_back();
        }
    }
}

/// Renders the main screens to PNGs with fake data, for checking the look without a database.
/// Run with `TABELLA_SHOTS=<dir> cargo test screenshots -- --ignored` (needs a Vulkan driver, e.g. lavapipe).
#[cfg(test)]
pub(crate) mod screenshots;

/// The main screens at small sizes and larger zoom, where layouts break first:
/// `TABELLA_SHOTS=<dir> cargo test ux_sweep -- --ignored` writes `<dir>/sweep/*.png`.
#[cfg(test)]
mod ux_sweep {
    use eframe::egui;
    use egui_kittest::Harness;

    use super::screenshots::fake_app;
    use crate::app::TabKind;

    #[test]
    #[ignore = "renders with wgpu (needs a Vulkan driver) and writes PNGs to TABELLA_SHOTS"]
    fn ux_sweep() {
        let dir = std::env::var("TABELLA_SHOTS").unwrap_or_else(|_| "target/shots".into());
        let out = format!("{dir}/sweep");
        std::fs::create_dir_all(&out).unwrap();
        for (w, h, zoom) in [(1024.0, 640.0, 1.0), (1366.0, 768.0, 1.25), (1920.0, 1080.0, 1.5)] {
            let mut harness = Harness::builder()
                .with_size(egui::vec2(w, h))
                .wgpu()
                .build_eframe(|cc| {
                    super::theme::apply(&cc.egui_ctx);
                    fake_app(&cc.egui_ctx)
                });
            harness.ctx.set_zoom_factor(zoom);
            let tag = format!("{}x{}@{}", w as u32, h as u32, (zoom * 100.0) as u32);
            let shot = |harness: &mut Harness<'_, crate::app::App>, name: &str| {
                harness.run();
                harness
                    .render()
                    .unwrap()
                    .save(format!("{out}/{tag}-{name}.png"))
                    .unwrap();
            };
            shot(&mut harness, "table");
            {
                let app = harness.state_mut();
                app.active = 1;
                if let Some(ed) = app.tabs[1].editor_mut() {
                    ed.text = "SELECT count(*) FROM customers;\n\nSELECT id, name FROM customers;".into();
                }
            }
            shot(&mut harness, "sql");
            harness.state_mut().open_compare(None);
            shot(&mut harness, "compare");
            harness.state_mut().open_dump(crate::app::DumpScope::Database);
            shot(&mut harness, "dump");
            harness.state_mut().open_history();
            shot(&mut harness, "history");
            harness.state_mut().open_search();
            shot(&mut harness, "search");
            {
                let app = harness.state_mut();
                app.active = 0;
                if let TabKind::Table(ts) = &mut app.tabs[0].kind {
                    ts.filter = "state IN ('NY', 'CA', 'TX', 'FL', 'WA', 'IL', 'GA', 'OH') AND balance > 1000 AND active AND email LIKE '%@example.com'".into();
                }
            }
            shot(&mut harness, "table-long-filter");
            harness.state_mut().session = None;
            harness.state_mut().tabs.clear();
            shot(&mut harness, "welcome");
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use eframe::egui;
    use egui_kittest::Harness;

    use super::screenshots::fake_app;

    /// The whole cell answers, not just its text: a double click in the empty part of a cell
    /// (right of a short value) starts editing it, a single one selects it.
    #[test]
    fn cells_answer_across_their_whole_width() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1280.0, 760.0))
            // Frames at 60 Hz, like the app: double clicks depend on real intervals.
            .with_step_dt(1.0 / 60.0)
            .build_eframe(|cc| {
                super::theme::apply(&cc.egui_ctx);
                fake_app(&cc.egui_ctx)
            });
        harness.run();
        use egui_kittest::kittest::Queryable;
        let text = harness.get_by_label("Customer 5").rect();
        // 40 points right of the text: still the "name" cell, nothing written there.
        let spot = egui::pos2(text.right() + 40.0, text.center().y);
        let click = |harness: &mut Harness<crate::app::App>| {
            for pressed in [true, false] {
                harness.event(egui::Event::PointerButton {
                    pos: spot,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                });
            }
        };
        harness.event(egui::Event::PointerMoved(spot));
        click(&mut harness);
        harness.run();
        assert_eq!(
            harness.state().tabs[0].selected,
            Some((4, 1)),
            "a click selects the cell"
        );
        // About 0.35 s later, a double click: egui reports it as a triple click (it pairs with
        // the click above, within 0.6 s); that must edit too.
        for _ in 0..20 {
            harness.step();
        }
        click(&mut harness);
        click(&mut harness);
        harness.run();
        let editing = match &harness.state().tabs[0].kind {
            crate::app::TabKind::Table(ts) => ts.edit.as_ref().map(|e| (e.row, e.col)),
            _ => None,
        };
        assert_eq!(editing, Some((4, 1)), "a double click there edits it");
    }

    /// The sidebar must keep its width: content wider than the panel makes it grow a little on
    /// every frame (which looked like a slow-motion animation on startup).
    #[test]
    fn sidebar_keeps_its_width() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1280.0, 760.0))
            .build_eframe(|cc| {
                super::theme::apply(&cc.egui_ctx);
                fake_app(&cc.egui_ctx)
            });
        let width = |h: &Harness<'_, crate::app::App>| {
            egui::containers::panel::PanelState::load(&h.ctx, egui::Id::new("side")).map(|p| p.size().x)
        };
        harness.run_steps(3);
        let first = width(&harness);
        harness.run_steps(30);
        assert_eq!(width(&harness), first, "sidebar width changed between frames");
    }
}

/// Draws the app icon (a database cylinder in the theme's lime on its dark background, square
/// corners like the rest of the UI) and writes `assets/icon.png` and `assets/icon.ico`.
/// Run with `cargo test make_icon -- --ignored` after changing it.
#[cfg(test)]
mod icon_asset {
    use eframe::egui::Color32;
    use image::codecs::ico::{IcoEncoder, IcoFrame};
    use image::{ImageBuffer, Rgba, imageops::FilterType};

    use super::theme;

    /// The icon at `size` px, 4x4 supersampled.
    fn draw(size: u32) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
        let t = theme::tokens();
        let bg = t.background;
        let top = theme::oklch(0.95, 0.19, 117.0);
        let body = theme::oklch(0.84, 0.18, 117.0);
        // Geometry on a 0..1 canvas.
        let (cx, rx, ry) = (0.5f32, 0.30f32, 0.095f32);
        let (y_top, y_bot) = (0.30f32, 0.72f32);
        let bands = [0.44f32, 0.58];
        let band_w = 0.028f32;
        let color_at = |x: f32, y: f32| -> Color32 {
            let dx = (x - cx) / rx;
            let in_x = dx.abs() <= 1.0;
            let arc = |y0: f32| y0 + ry * (1.0 - dx * dx).max(0.0).sqrt();
            let in_ellipse = |y0: f32| dx * dx + ((y - y0) / ry).powi(2) <= 1.0;
            if in_ellipse(y_top) {
                // The lid, with a thin dark rim near its edge.
                let r = dx * dx + ((y - y_top) / ry).powi(2);
                return if r > 0.78 && y > y_top { body } else { top };
            }
            let in_body = in_x && ((y >= y_top && y <= y_bot) || in_ellipse(y_bot));
            if !in_body {
                return bg;
            }
            if bands.iter().any(|&b| y >= arc(b) && y < arc(b) + band_w) {
                return bg;
            }
            body
        };
        let ss = 4;
        ImageBuffer::from_fn(size, size, |px, py| {
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let x = (px as f32 + (sx as f32 + 0.5) / ss as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / ss as f32) / size as f32;
                    let c = color_at(x, y);
                    r += c.r() as u32;
                    g += c.g() as u32;
                    b += c.b() as u32;
                }
            }
            let n = (ss * ss) as u32;
            Rgba([(r / n) as u8, (g / n) as u8, (b / n) as u8, 255])
        })
    }

    #[test]
    #[ignore = "rewrites assets/icon.png and icon.ico"]
    fn make_icon() {
        let big = draw(256);
        big.save("assets/icon.png").unwrap();
        let frames: Vec<IcoFrame> = [16u32, 24, 32, 48, 64, 128, 256]
            .iter()
            .map(|&s| {
                // Small sizes drawn directly stay crisper than a downscale of the big one.
                let img = if s >= 64 {
                    image::imageops::resize(&big, s, s, FilterType::Lanczos3)
                } else {
                    draw(s)
                };
                IcoFrame::as_png(img.as_raw(), s, s, image::ExtendedColorType::Rgba8).unwrap()
            })
            .collect();
        let file = std::fs::File::create("assets/icon.ico").unwrap();
        IcoEncoder::new(file).encode_images(&frames).unwrap();
    }
}
