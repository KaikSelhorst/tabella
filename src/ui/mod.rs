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
mod plan_view;
mod prefs;
mod sidebar;
mod structure;
mod syntax;
mod tabs;
pub mod theme;
mod welcome;

use eframe::egui;

use crate::app::App;

pub use egui_phosphor::regular as icon;

impl App {
    /// Small FPS readout in the bottom-left corner (under the sidebar); click it to toggle max-rate redraw.
    fn fps_overlay(&mut self, ctx: &egui::Context) {
        let t = theme::tokens();
        egui::Area::new(egui::Id::new("fps"))
            .anchor(egui::Align2::LEFT_BOTTOM, [8.0, -4.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                // Off by default: frames are only drawn when something changes, so the count
                // means little until max-rate redraw is switched on.
                let (label, color) = if self.max_fps {
                    (format!("{:.0} FPS", self.fps), t.warning)
                } else {
                    ("FPS".to_string(), t.subtle)
                };
                let text = egui::RichText::new(label).small().color(color);
                let r = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                if r.clicked() {
                    self.max_fps = !self.max_fps;
                }
                r.on_hover_text(if self.max_fps {
                    "Redesenhando na taxa do monitor. Clique para economizar CPU."
                } else {
                    "Redesenho sob demanda. Clique para usar a taxa máxima."
                });
            });
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        crate::session::save(&self.snapshot());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.tick_fps();
        self.clear_finished_notice();
        self.window = ui
            .ctx()
            .input(|i| i.viewport().inner_rect)
            .map(|r| [r.width(), r.height()]);
        if self.max_fps {
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
        let ctx = ui.ctx().clone();
        self.connection_form(&ctx);
        self.palette_ui(&ctx);
        self.master_prompt(&ctx);
        self.favorite_prompt(&ctx);
        self.help_ui(&ctx);
        self.path_prompt_ui(&ctx);
        self.table_action_ui(&ctx);
        self.track_zoom(&ctx);
    }
}

impl App {
    /// "Transação aberta" across the window while a manual transaction has uncommitted work.
    fn tx_band(&mut self, ui: &mut egui::Ui) {
        let Some(txn) = self.session.as_ref().map(|s| s.txn).filter(|t| t.open) else {
            return;
        };
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
                        "Transação aberta com erro: um comando falhou, só é possível desfazer".to_string()
                    } else if txn.writes == 0 {
                        "Transação aberta: nada foi gravado ainda".to_string()
                    } else {
                        format!(
                            "Transação aberta · {} comando(s) de escrita ainda não confirmados",
                            txn.writes
                        )
                    };
                    ui.label(egui::RichText::new(text).color(t.foreground));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!txn.failed, theme::primary_button(Some(icon::CHECK), "Confirmar"))
                            .on_hover_text("COMMIT: grava tudo")
                            .clicked()
                        {
                            end = Some(true);
                        }
                        if ui
                            .add(theme::outline_button("Desfazer"))
                            .on_hover_text("ROLLBACK: descarta tudo desde o início da transação")
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
        let Some(p) = &mut self.table_action else { return };
        let Some(s) = &self.session else {
            self.table_action = None;
            return;
        };
        let t = theme::tokens();
        let rows = s
            .tables
            .iter()
            .find(|x| x.schema == p.schema && x.name == p.table)
            .and_then(|x| x.estimate)
            .map(|n| format!("~{} linhas", palette::format_count(n)))
            .unwrap_or_else(|| "linhas".into());
        let destructive = matches!(p.action, TableAction::Truncate | TableAction::Drop);
        let (title, verb) = match p.action {
            TableAction::Rename => ("Renomear tabela", "Renomear"),
            TableAction::Duplicate(_) => ("Duplicar tabela", "Duplicar"),
            TableAction::Truncate => ("Esvaziar tabela", "Esvaziar"),
            TableAction::Drop => ("Excluir tabela", "Excluir"),
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
                    TableAction::Rename => "Novo nome:".to_string(),
                    TableAction::Duplicate(_) => "Nome da cópia (mesmas colunas, índices e padrões):".to_string(),
                    TableAction::Truncate => format!(
                        "Apaga todas as {rows} de {}. A estrutura fica. Não dá para desfazer (a não ser em commit manual).",
                        p.table
                    ),
                    TableAction::Drop => format!(
                        "Apaga {} inteira: estrutura, índices e todas as {rows}. Não dá para desfazer (a não ser em commit manual).",
                        p.table
                    ),
                };
                let hint_color = if destructive { t.destructive } else { t.muted_foreground };
                ui.add(egui::Label::new(egui::RichText::new(hint).color(hint_color)).wrap());
                if destructive {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(format!("Digite {} para confirmar:", p.table)).size(12.0).color(t.subtle));
                }
                let r = ui.add(
                    egui::TextEdit::singleline(&mut p.input)
                        .font(egui::FontId::monospace(12.5))
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
                if let TableAction::Duplicate(with_data) = &mut p.action {
                    ui.checkbox(with_data, format!("copiar as linhas também ({rows})"));
                }
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(t.sidebar)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let job = syntax::highlight(&sql, egui::FontId::monospace(12.0));
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
                    if ui.add(theme::outline_button("Cancelar")).clicked() {
                        cancel = true;
                    }
                });
            });
        if go {
            if let Some(p) = self.table_action.take() {
                self.run_table_action(p);
            }
        } else if cancel || modal.should_close() {
            self.table_action = None;
        }
    }

    /// Where there's no native file dialog: type the path.
    fn path_prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.path_prompt else { return };
        let t = theme::tokens();
        let (mut ok, mut cancel) = (false, false);
        let saving = p.save_tab.is_some();
        let modal = egui::Modal::new(egui::Id::new("path_prompt"))
            .frame(theme::modal_frame(16))
            .show(ctx, |ui| {
                ui.set_width(480.0);
                ui.label(
                    egui::RichText::new(if saving {
                        "Salvar como"
                    } else {
                        "Abrir arquivo SQL"
                    })
                    .size(15.0)
                    .color(t.foreground),
                );
                ui.add_space(8.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut p.path)
                        .hint_text("/caminho/do/arquivo.sql")
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
                        .add(theme::primary_button(
                            None,
                            if saving { "Salvar" } else { "Abrir" },
                        ))
                        .clicked()
                    {
                        ok = true;
                    }
                    if ui.add(theme::outline_button("Cancelar")).clicked() {
                        cancel = true;
                    }
                });
            });
        if ok && !p.path.trim().is_empty() {
            let p = self.path_prompt.take().expect("open prompt");
            let path = std::path::PathBuf::from(p.path.trim());
            match p
                .save_tab
                .and_then(|id| self.tabs.iter().position(|t| t.id == id))
            {
                Some(i) => self.save_file_as(i, path),
                None if p.run => self.open_script(path),
                None => self.open_file(path),
            }
        } else if cancel || modal.should_close() {
            self.path_prompt = None;
        }
    }

    /// Closing the window would silently roll back an open transaction: ask first.
    fn quit_guard(&mut self, ctx: &egui::Context) {
        let open = self.session.as_ref().is_some_and(|s| s.txn.open);
        if open && !self.quit_after_tx && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.quit_prompt = true;
        }
        if !self.quit_prompt {
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
                        egui::RichText::new("Transação aberta")
                            .size(15.0)
                            .color(t.foreground),
                    );
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Há alterações não confirmadas. Fechar sem decidir desfaz todas elas.",
                    )
                    .color(t.muted_foreground),
                );
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(theme::primary_button(Some(icon::CHECK), "Confirmar e sair"))
                        .clicked()
                    {
                        choice = Some(Some(true));
                    }
                    if ui.add(theme::outline_button("Desfazer e sair")).clicked() {
                        choice = Some(Some(false));
                    }
                    if ui.add(theme::outline_button("Voltar")).clicked() {
                        choice = Some(None);
                    }
                });
            });
        if modal.should_close() && choice.is_none() {
            choice = Some(None);
        }
        match choice {
            Some(Some(commit)) => {
                self.quit_prompt = false;
                self.quit_after_tx = true;
                self.end_tx(commit);
            }
            Some(None) => self.quit_prompt = false,
            None => {}
        }
    }

    /// App-wide keys. Consumed here so text fields don't also react to them.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F1)) {
            self.help = !self.help;
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
        let (palette, new_sql, close, prev, next) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::P) || i.consume_key(Modifiers::COMMAND, Key::K),
                i.consume_key(Modifiers::COMMAND, Key::T),
                i.consume_key(Modifiers::COMMAND, Key::W),
                // Shift variant first: the plain one would also match Ctrl+Shift+Tab.
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Tab),
                i.consume_key(Modifiers::COMMAND, Key::Tab),
            )
        });
        if palette {
            self.palette = if self.palette.is_some() {
                None
            } else {
                Some(Default::default())
            };
        }
        let ready = self.session.as_ref().is_some_and(|s| s.ready);
        if new_sql && ready {
            self.open_sql(String::new());
        }
        if close {
            self.close_tab(self.active);
        }
        let n = self.tabs.len();
        if n > 0 && next {
            self.active = (self.active + 1) % n;
        }
        if n > 0 && prev {
            self.active = (self.active + n - 1) % n;
        }
    }
}

/// Renders the main screens to PNGs with fake data, for checking the look without a database.
/// Run with `DBLITE_SHOTS=<dir> cargo test screenshots -- --ignored` (needs a Vulkan driver, e.g. lavapipe).
#[cfg(test)]
mod screenshots {
    use std::time::Duration;

    use eframe::egui;
    use egui_kittest::Harness;

    use crate::app::{App, Tab, TabKind, TableState};
    use crate::db::{ColumnInfo, ConnConfig, Kind, QueryResult, TableRef};

    pub fn fake_app(ctx: &egui::Context) -> App {
        // Keep the real config untouched: App::new reads (and may migrate) it.
        let dir = std::env::temp_dir().join("dblite-screenshots-config");
        // SAFETY: set before any other thread reads the environment in this test binary's run.
        unsafe { std::env::set_var("DBLITE_CONFIG_DIR", &dir) };
        let mut app = App::new(ctx);
        app.max_fps = false;
        let mut cfg = ConnConfig::new(Kind::Postgres);
        cfg.id = "dev".into();
        cfg.name = "dev".into();
        app.saved = vec![cfg.clone()];
        let table = |schema: &str, name: &str, is_view| TableRef {
            schema: schema.into(),
            name: name.into(),
            is_view,
            estimate: (!is_view).then_some(name.len() as i64 * 1_337),
            search_key: name.to_lowercase(),
            size: (!is_view).then_some(name.len() as i64 * 9_437_184),
        };
        let (mut session, _, replies) = crate::app::tests::fake_session(Kind::Postgres);
        // The screenshots need a live session for the whole test.
        std::mem::forget(replies);
        session.cfg = cfg;
        session.tables = vec![
            table("public", "clientes", false),
            table("public", "pedidos", false),
            table("public", "produtos", false),
            table("public", "vendas_mensais", true),
            table("auth", "sessions", false),
            table("auth", "users", false),
        ];
        session.columns = ["id", "nome", "email", "ativo", "saldo", "criado_em"]
            .iter()
            .map(|c| ("clientes".to_string(), c.to_string()))
            .chain([("pedidos".to_string(), "cliente_id".to_string())])
            .collect();
        session.databases = vec!["dev".into(), "postgres".into()];
        session.current_db = "postgres".into();
        session.current_schema = "public".into();
        app.session = Some(session);
        let now = crate::history::now();
        let entry = |sql: &str, ago: u64| crate::history::Entry {
            conn: "dev".into(),
            sql: sql.into(),
            at: now - ago,
            ok: true,
            ms: 3,
        };
        app.history = vec![
            entry("SELECT count(*) FROM clientes WHERE ativo", 7200),
            entry("SELECT * FROM pedidos WHERE cliente_id = 3", 600),
        ];
        let col = |name: &str, ty: &str, pk| ColumnInfo {
            name: name.into(),
            data_type: ty.into(),
            pk,
            nullable: !pk,
            has_default: false,
            references: None,
        };
        let cols = vec![
            col("id", "integer", true),
            col("nome", "text", false),
            col("email", "text", false),
            col("ativo", "boolean", false),
            col("saldo", "numeric", false),
            col("criado_em", "timestamp without time zone", false),
        ];
        let rows = (1..=40)
            .map(|i| {
                vec![
                    Some(i.to_string()),
                    Some(format!("Cliente {i}")),
                    Some(format!("cliente{i}@exemplo.com")),
                    Some(if i % 3 == 0 { "f" } else { "t" }.into()),
                    if i % 5 == 0 {
                        None
                    } else {
                        Some(format!("{}.{:02}", i * 37, i % 100))
                    },
                    Some(format!("2026-10-{:02} 14:{:02}:07", i % 28 + 1, i % 60)),
                ]
            })
            .collect();
        let result = QueryResult {
            columns: cols.iter().map(|c| c.name.clone()).collect(),
            rows,
            affected: None,
            truncated: false,
            elapsed: Duration::from_millis(3),
            previous: Vec::new(),
        };
        let mut ts = TableState::new("public", "clientes", Some(40));
        ts.cols = cols;
        ts.order = Some(("id".into(), true));
        ts.total = Some(40);
        ts.counted_filter = Some(String::new());
        let mut table_tab = Tab::new(1, "clientes".into(), TabKind::Table(Box::new(ts)));
        table_tab.result = Some(result.clone());
        table_tab.selected = Some((2, 2));
        let sql = "SELECT id, nome, saldo\nFROM public.clientes\nWHERE ativo\nORDER BY saldo DESC;";
        let mut sql_tab = Tab::new(
            2,
            "SQL 1".into(),
            TabKind::Sql(Box::new(crate::app::Editor::new(sql.into()))),
        );
        sql_tab.result = Some(result);
        app.tabs = vec![table_tab, sql_tab];
        app.active = 0;
        app
    }

    #[test]
    #[ignore]
    fn screenshots() {
        let dir = std::env::var("DBLITE_SHOTS").unwrap_or_else(|_| "target/shots".into());
        std::fs::create_dir_all(&dir).unwrap();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1280.0, 760.0))
            .wgpu()
            .build_eframe(|cc| {
                super::theme::apply(&cc.egui_ctx);
                fake_app(&cc.egui_ctx)
            });
        harness.run();
        let img = harness.render().unwrap();
        img.save(format!("{dir}/table.png")).unwrap();

        // New features: environment band + export banner, insert form, structure tab.
        harness.state_mut().session.as_mut().unwrap().cfg.color = Some(crate::db::Tag::Red);
        harness.state_mut().tabs[0].exported =
            Some(("/home/u/Downloads/clientes_20261013-190000.csv".into(), 500));
        // `run` fails if the UI keeps asking to repaint: this layout used to make the scroll bars
        // flip-flop forever.
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/band-export.png"))
            .unwrap();
        harness.state_mut().tabs[0].exported = None;
        crate::app::open_insert_form(&mut harness.state_mut().tabs[0]);
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/insert.png"))
            .unwrap();
        if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
            ts.insert = None;
        }
        // CSV import dialog with a semicolon file from a spreadsheet.
        {
            let csv = std::env::temp_dir().join("dblite-shot-clientes.csv");
            std::fs::write(
                &csv,
                "Nome;Email;Ativo;Saldo;Observação\nMaria Souza;maria@exemplo.com;sim;1520,30;cliente antiga\nJoão Lima;joao@exemplo.com;não;;\nAna Prado;ana@exemplo.com;sim;88,00;indicação\n",
            )
            .unwrap();
            crate::app::open_import(&mut harness.state_mut().tabs[0], Some(csv.display().to_string()));
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/import.png"))
                .unwrap();
            if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
                ts.import = None;
            }
            let _ = std::fs::remove_file(csv);
        }
        harness.state_mut().session.as_mut().unwrap().cfg.color = None;
        {
            use crate::db::{StructColumn, TableStructure};
            let col = |name: &str, ty: &str, nullable, default: Option<&str>, pk| StructColumn {
                name: name.into(),
                data_type: ty.into(),
                nullable,
                default: default.map(Into::into),
                pk,
            };
            let data = TableStructure {
                columns: vec![
                    col("id", "integer", false, Some("nextval('clientes_id_seq'::regclass)"), true),
                    col("nome", "text", true, None, false),
                    col("email", "text", true, None, false),
                    col("ativo", "boolean", true, Some("true"), false),
                    col("saldo", "numeric(12,2)", true, None, false),
                    col("criado_em", "timestamp without time zone", true, Some("now()"), false),
                ],
                indexes: vec![
                    ("clientes_pkey".into(), "CREATE UNIQUE INDEX clientes_pkey ON public.clientes USING btree (id)".into()),
                    ("clientes_email_idx".into(), "CREATE INDEX clientes_email_idx ON public.clientes USING btree (email)".into()),
                ],
                foreign_keys: vec![],
                ddl: "CREATE TABLE \"public\".\"clientes\" (\n    \"id\" integer NOT NULL DEFAULT nextval('clientes_id_seq'::regclass),\n    \"nome\" text,\n    \"email\" text,\n    \"ativo\" boolean DEFAULT true,\n    \"saldo\" numeric(12,2),\n    \"criado_em\" timestamp without time zone DEFAULT now(),\n    CONSTRAINT \"clientes_pkey\" PRIMARY KEY (id)\n);\nCREATE INDEX clientes_email_idx ON public.clientes USING btree (email);".into(),
            };
            let st = crate::app::StructState {
                schema: "public".into(),
                table: "clientes".into(),
                data: Some(data),
            };
            let tab = Tab::new(3, "clientes · estrutura".into(), TabKind::Structure(Box::new(st)));
            harness.state_mut().tabs.push(tab);
            harness.state_mut().active = 2;
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/structure.png"))
                .unwrap();
            // Table designer from that structure, with a few edits pending.
            let data = match &harness.state().tabs[2].kind {
                TabKind::Structure(st) => st.data.clone().unwrap(),
                _ => unreachable!(),
            };
            harness.state_mut().open_design("public", "clientes", &data);
            if let Some(TabKind::Design(d)) = harness.state_mut().tabs.last_mut().map(|t| &mut t.kind) {
                d.columns[1].name = "nome_completo".into();
                d.columns[1].nullable = false;
                d.columns[4].dropped = true;
                d.columns.push(crate::design::ColDef {
                    name: "uf".into(),
                    data_type: "char(2)".into(),
                    default: "'SP'".into(),
                    ..crate::design::ColDef::new()
                });
                d.indexes.push(crate::design::IndexDef {
                    existing: false,
                    name: String::new(),
                    columns: "uf, criado_em".into(),
                    unique: false,
                    dropped: false,
                });
            }
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/design.png"))
                .unwrap();
            harness.state_mut().tabs.truncate(2);
            harness.state_mut().active = 0;

            // Keyboard shortcuts (F1).
            harness.state_mut().help = true;
            harness.run();
            harness.render().unwrap().save(format!("{dir}/help.png")).unwrap();
            harness.state_mut().help = false;

            // "Duplicar linha": the insert form filled from row 3.
            crate::app::open_duplicate_form(&mut harness.state_mut().tabs[0], 2);
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/duplicate.png"))
                .unwrap();
            if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
                ts.insert = None;
            }
        }
        // 3x zoom on the grid header's left corner, to check pixel-level alignment.
        let crop = image::imageops::crop_imm(&img, 236, 64, 420, 80).to_image();
        image::imageops::resize(&crop, 1260, 240, image::imageops::FilterType::Nearest)
            .save(format!("{dir}/header-zoom.png"))
            .unwrap();
        let crop = image::imageops::crop_imm(&img, 0, 0, 260, 110).to_image();
        image::imageops::resize(&crop, 780, 330, image::imageops::FilterType::Nearest)
            .save(format!("{dir}/sidebar-zoom.png"))
            .unwrap();
        let crop = image::imageops::crop_imm(&img, 236, 32, 1044, 48).to_image();
        image::imageops::resize(&crop, 2088, 96, image::imageops::FilterType::Nearest)
            .save(format!("{dir}/toolbar-zoom.png"))
            .unwrap();

        // Activity monitor (auto refresh off: the fake session never answers) + sidebar by size.
        {
            let cols = [
                "pid", "usuario", "banco", "estado", "duracao", "espera", "cliente", "consulta",
            ];
            let row = |pid: &str, st: &str, dur: &str, q: &str| {
                [pid, "postgres", "dev", st, dur, "", "172.18.0.1", q]
                    .map(|v| (!v.is_empty()).then(|| v.to_string()))
                    .to_vec()
            };
            let res = QueryResult {
                columns: cols.iter().map(|c| c.to_string()).collect(),
                rows: vec![
                    row(
                        "4121",
                        "active",
                        "00:00:41",
                        "SELECT count(*) FROM pedidos p JOIN clientes c ON c.id = p.cliente_id",
                    ),
                    row(
                        "4188",
                        "idle in transaction",
                        "00:03:12",
                        "UPDATE clientes SET saldo = 0 WHERE id = 7",
                    ),
                    row("4203", "idle", "00:12:55", "SELECT 1"),
                ],
                affected: None,
                truncated: false,
                elapsed: Duration::from_millis(2),
                previous: Vec::new(),
            };
            let st = crate::app::ActivityState {
                view: crate::db::ActivityView::Sessions,
                server: None,
                auto_refresh: false,
                last_refresh: None,
                confirm_kill: None,
                kill_error: None,
            };
            let mut tab = Tab::new(4, "Atividade".into(), TabKind::Activity(Box::new(st)));
            tab.result = Some(res);
            tab.selected = Some((1, 0));
            let state = harness.state_mut();
            state.tabs.push(tab);
            state.active = 2;
            state.sort_by_size = true;
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/activity.png"))
                .unwrap();
            if let TabKind::Activity(st) = &mut harness.state_mut().tabs[2].kind {
                st.confirm_kill = Some((4188, true));
            }
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/activity-kill.png"))
                .unwrap();
            if let TabKind::Activity(st) = &mut harness.state_mut().tabs[2].kind {
                st.confirm_kill = None;
                st.view = crate::db::ActivityView::Slow;
            }
            let slow = QueryResult {
                columns: ["chamadas", "total_ms", "media_ms", "linhas", "consulta"].map(String::from).to_vec(),
                rows: [
                    ["1240", "98213.4", "79.20", "1240", "SELECT count(*) FROM pedidos p JOIN clientes c ON c.id = p.cliente_id WHERE p.status = $1"],
                    ["58201", "41022.8", "0.70", "58201", "SELECT * FROM clientes WHERE email = $1"],
                    ["12", "8890.1", "740.84", "60000000", "SELECT * FROM eventos ORDER BY criado_em DESC"],
                ]
                .iter()
                .map(|r| r.iter().map(|v| Some(v.to_string())).collect())
                .collect(),
                ..Default::default()
            };
            harness.state_mut().tabs[2].result = Some(slow);
            harness.state_mut().tabs[2].selected = Some((0, 4));
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/activity-slow.png"))
                .unwrap();

            let state = harness.state_mut();
            state.tabs.pop();
            state.sort_by_size = false;
        }

        harness.state_mut().active = 1;
        harness.run();
        harness.render().unwrap().save(format!("{dir}/sql.png")).unwrap();

        // EXPLAIN ANALYZE of a join with a big full scan.
        {
            let json = r#"[{"Plan": {"Node Type": "Aggregate", "Total Cost": 98000, "Plan Rows": 1, "Actual Total Time": 812.0, "Actual Rows": 1, "Actual Loops": 1,
              "Plans": [{"Node Type": "Hash Join", "Total Cost": 97000, "Plan Rows": 1200, "Actual Total Time": 790.0, "Actual Rows": 41000, "Actual Loops": 1,
                "Hash Cond": "(p.cliente_id = c.id)",
                "Plans": [
                  {"Node Type": "Seq Scan", "Relation Name": "pedidos", "Alias": "p", "Total Cost": 91000, "Plan Rows": 5000000, "Actual Total Time": 610.0, "Actual Rows": 5000000, "Actual Loops": 1, "Filter": "(status = 'pago'::text)", "Rows Removed by Filter": 4170000},
                  {"Node Type": "Hash", "Total Cost": 3100, "Plan Rows": 900, "Actual Total Time": 21.0, "Actual Rows": 900, "Actual Loops": 1,
                    "Plans": [{"Node Type": "Index Scan", "Index Name": "clientes_pkey", "Relation Name": "clientes", "Alias": "c", "Total Cost": 3000, "Plan Rows": 900, "Actual Total Time": 18.0, "Actual Rows": 900, "Actual Loops": 1, "Index Cond": "(id < 1000)"}]}
                ]}]},
              "Planning Time": 0.4, "Execution Time": 812.5}]"#;
            let tab = &mut harness.state_mut().tabs[1];
            tab.editor_mut().unwrap().plan = Some(crate::plan::from_pg_json(json).unwrap());
            harness.run();
            harness.render().unwrap().save(format!("{dir}/plan.png")).unwrap();
            harness.state_mut().tabs[1].editor_mut().unwrap().plan = None;
        }

        // Wide editor result scrolled sideways: "id" and "nome" are pinned and stay at the left.
        {
            let state = harness.state_mut();
            state.active = 1;
            let tab = &mut state.tabs[1];
            let saved = tab.result.take();
            let names: Vec<String> = ["id", "nome"]
                .iter()
                .map(|s| s.to_string())
                .chain((1..=14).map(|i| format!("campo_{i:02}")))
                .collect();
            tab.result = Some(QueryResult {
                rows: (1..=30)
                    .map(|r| {
                        names
                            .iter()
                            .enumerate()
                            .map(|(i, _)| match i {
                                0 => Some(r.to_string()),
                                1 => Some(format!("Cliente {r}")),
                                _ => Some(format!("valor {r}-{i}")),
                            })
                            .collect()
                    })
                    .collect(),
                columns: names,
                ..Default::default()
            });
            tab.pinned_cols = vec!["id".into(), "nome".into()];
            tab.hidden_cols = vec!["campo_03".into()];
            tab.selected = Some((4, 9));
            harness.run();
            harness.hover_at(egui::pos2(700.0, 500.0));
            for _ in 0..3 {
                harness.event(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(-400.0, 0.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                });
                harness.step();
            }
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/pinned.png"))
                .unwrap();
            let tab = &mut harness.state_mut().tabs[1];
            tab.result = saved;
            tab.pinned_cols.clear();
            tab.hidden_cols.clear();
            tab.selected = None;
        }

        // Several statements: the one under the cursor is marked, and each query has a result.
        {
            let editor = egui::Id::new(("sql", 2u64));
            let script = "SELECT count(*) FROM clientes;\n\nSELECT id, nome, saldo\nFROM clientes\nWHERE ativo;\n\nSELECT * FROM pedidos LIMIT 5;";
            let state = harness.state_mut();
            let tab = &mut state.tabs[1];
            let mut old = script.to_string();
            if let Some(text) = tab.editor_mut().map(|ed| &mut ed.text) {
                std::mem::swap(text, &mut old);
            }
            let main = tab.result.clone().unwrap();
            let small = |col: &str, n: usize| QueryResult {
                columns: vec![col.into()],
                rows: (0..n).map(|i| vec![Some(i.to_string())]).collect(),
                ..Default::default()
            };
            tab.sets = vec![small("count", 1), QueryResult::default(), small("id", 5)];
            tab.set_shown = 1;
            tab.result = Some(main);
            let mut st = egui::TextEdit::load_state(&harness.ctx, editor).unwrap_or_default();
            st.cursor
                .set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(45))));
            st.store(&harness.ctx, editor);
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/sql-multi.png"))
                .unwrap();
            let tab = &mut harness.state_mut().tabs[1];
            tab.sets.clear();
            if let Some(text) = tab.editor_mut().map(|ed| &mut ed.text) {
                *text = old;
            }
        }

        // Open manual transaction + column statistics over the table tab.
        {
            let state = harness.state_mut();
            state.active = 0;
            state.session.as_mut().unwrap().txn = crate::worker::TxState {
                manual: true,
                open: true,
                writes: 3,
                failed: false,
            };
            state.tabs[0].col_stats = Some(crate::app::StatsView {
                column: "cidade".into(),
                scope: "tabela inteira".into(),
                data: Some(Ok(crate::values::ColumnStats {
                    total: 1_000_000,
                    nulls: 0,
                    distinct: 10,
                    min: None,
                    max: None,
                    top: [
                        "São Paulo",
                        "Rio de Janeiro",
                        "Belo Horizonte",
                        "Curitiba",
                        "Recife",
                    ]
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (Some(c.to_string()), 100_000 - i as i64 * 7_000))
                    .collect(),
                })),
            });
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/tx-stats.png"))
                .unwrap();
            let state = harness.state_mut();
            state.tabs[0].col_stats = None;
            state.session.as_mut().unwrap().txn = Default::default();
        }

        // Editor: find bar, then the parameters prompt, then a JSON value in the detail panel.
        {
            let state = harness.state_mut();
            state.active = 1;
            if let Some(text) = state.tabs[1].editor_mut().map(|ed| &mut ed.text) {
                *text = "SELECT *\nFROM clientes\nWHERE id = :id\n  AND cidade = :cidade\n  AND id IN (SELECT cliente_id FROM pedidos);".into();
            }
            let saved = state.tabs[1].result.take();
            state.tabs[1].result = Some(QueryResult {
                columns: vec!["id".into(), "payload".into()],
                rows: vec![vec![
                    Some("7".into()),
                    Some(r#"{"pagina": "/produto/42", "navegador": "firefox", "itens": [{"sku": "A-1", "preco": 19.9}, {"sku": "B-2", "preco": 5}], "cupom": null, "novo": true}"#.into()),
                ]],
                ..Default::default()
            });
            state.tabs[1].selected = Some((0, 1));
            harness.ctx.data_mut(|d| {
                d.insert_temp(
                    egui::Id::new(("find", 2u64)),
                    super::find::FindState {
                        query: "id".into(),
                        current: 1,
                        show_replace: true,
                        ..Default::default()
                    },
                )
            });
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/find-json.png"))
                .unwrap();
            harness
                .ctx
                .data_mut(|d| d.remove::<super::find::FindState>(egui::Id::new(("find", 2u64))));
            let tab = &mut harness.state_mut().tabs[1];
            tab.editor_mut()
                .unwrap()
                .param_values
                .insert("id".into(), "42".into());
            tab.editor_mut().unwrap().params = Some(crate::app::ParamPrompt {
                sql: String::new(),
                names: vec!["id".into(), "cidade".into()],
                mode: crate::app::RunMode::Run,
                focused: true,
            });
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/params.png"))
                .unwrap();
            let tab = &mut harness.state_mut().tabs[1];
            tab.editor_mut().unwrap().params = None;
            tab.result = saved;
            tab.selected = None;
            if let Some(text) = tab.editor_mut().map(|ed| &mut ed.text) {
                *text =
                    "SELECT id, nome, saldo\nFROM public.clientes\nWHERE ativo\nORDER BY saldo DESC;".into();
            }
        }

        // Quick chart (bars, then a line over dates) and the UPDATE preview.
        {
            let state = harness.state_mut();
            state.active = 1;
            let tab = &mut state.tabs[1];
            let saved = tab.result.take();
            let cities = [
                "São Paulo",
                "Rio de Janeiro",
                "Belo Horizonte",
                "Curitiba",
                "Porto Alegre",
                "Recife",
                "Salvador",
                "Fortaleza",
            ];
            tab.result = Some(QueryResult {
                columns: vec!["cidade".into(), "total".into()],
                rows: cities
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        vec![
                            Some(c.to_string()),
                            Some(format!("{}.50", 182_000 - i * 17_500 - (i % 3) * 9_000)),
                        ]
                    })
                    .collect(),
                ..Default::default()
            });
            let chart_id = egui::Id::new(("chart", 2u64));
            harness.ctx.data_mut(|d| {
                d.insert_temp(
                    chart_id,
                    Some(super::chart::ChartSpec {
                        x: 0,
                        y: 1,
                        kind: super::chart::ChartKind::Bars,
                    }),
                )
            });
            harness.run();
            harness.hover_at(egui::pos2(480.0, 450.0));
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/chart-bars.png"))
                .unwrap();
            let tab = &mut harness.state_mut().tabs[1];
            tab.result = Some(QueryResult {
                columns: vec!["dia".into(), "pedidos".into()],
                rows: (1..=60)
                    .map(|d: i32| {
                        let v = 900 + (d * 37 % 200) + d * 12 - if d % 7 == 0 { 300 } else { 0 };
                        vec![
                            Some(format!("2026-{:02}-{:02}", 7 + (d - 1) / 31, (d - 1) % 31 + 1)),
                            Some(v.to_string()),
                        ]
                    })
                    .collect(),
                ..Default::default()
            });
            harness.ctx.data_mut(|d| {
                d.insert_temp(
                    chart_id,
                    Some(super::chart::ChartSpec {
                        x: 0,
                        y: 1,
                        kind: super::chart::ChartKind::Line,
                    }),
                )
            });
            harness.run();
            harness.hover_at(egui::pos2(800.0, 430.0));
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/chart-line.png"))
                .unwrap();
            harness
                .ctx
                .data_mut(|d| d.insert_temp::<Option<super::chart::ChartSpec>>(chart_id, None));

            let tab = &mut harness.state_mut().tabs[1];
            tab.editor_mut().unwrap().preview = Some(crate::app::PreviewState {
                sql: "UPDATE clientes SET ativo = false\nWHERE uf = 'SP' AND criado_em < '2023-01-01'".into(),
                data: Some(Ok(crate::db::PreviewData {
                    count: 18_342,
                    sample: QueryResult {
                        columns: vec![
                            "id".into(),
                            "nome".into(),
                            "uf".into(),
                            "ativo".into(),
                            "criado_em".into(),
                        ],
                        rows: (0..12)
                            .map(|i| {
                                vec![
                                    Some((i * 10 + 3).to_string()),
                                    Some(format!("Cliente {}", i * 10 + 3)),
                                    Some("SP".into()),
                                    Some("t".into()),
                                    Some(format!("2022-0{}-1{} 10:00:00", i % 9 + 1, i % 9)),
                                ]
                            })
                            .collect(),
                        ..Default::default()
                    },
                    after: false,
                })),
            });
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/preview.png"))
                .unwrap();
            let tab = &mut harness.state_mut().tabs[1];
            tab.editor_mut().unwrap().preview = None;
            tab.result = saved;
        }

        // Editable editor result (one table, key selected) with a pending edit; then a plan
        // with an index suggestion; then the server panel.
        {
            let state = harness.state_mut();
            state.active = 1;
            let tab = &mut state.tabs[1];
            let saved = tab.result.take();
            tab.result = Some(QueryResult {
                columns: vec!["id".into(), "nome".into(), "saldo".into()],
                rows: (1..=12)
                    .map(|i| {
                        vec![
                            Some(i.to_string()),
                            Some(format!("Cliente {i}")),
                            Some(format!("{}.00", i * 120)),
                        ]
                    })
                    .collect(),
                ..Default::default()
            });
            let mut src = crate::app::TableState::new("public", "clientes", None);
            let col = |name: &str, ty: &str, pk| ColumnInfo {
                name: name.into(),
                data_type: ty.into(),
                pk,
                nullable: !pk,
                has_default: false,
                references: None,
            };
            src.cols = vec![
                col("id", "integer", true),
                col("nome", "text", false),
                col("saldo", "numeric", false),
            ];
            src.changes.push(crate::app::Change::Set {
                key: vec![Some("3".into())],
                col: "nome".into(),
                value: Some("Cliente Três".into()),
                before: Some("Cliente 3".into()),
            });
            tab.editor_mut().unwrap().source = Some(Box::new(src));
            tab.editor_mut().unwrap().source_query =
                "SELECT id, nome, saldo FROM clientes WHERE ativo".into();
            tab.selected = Some((2, 1));
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/sql-edit.png"))
                .unwrap();
            let tab = &mut harness.state_mut().tabs[1];
            tab.editor_mut().unwrap().source = None;
            tab.selected = None;

            let json = r#"[{"Plan": {"Node Type": "Seq Scan", "Relation Name": "clientes", "Alias": "clientes", "Total Cost": 21000, "Plan Rows": 31000, "Filter": "((saldo > '1000'::numeric) AND (ativo = true))"}}]"#;
            tab.editor_mut().unwrap().plan = Some(crate::plan::from_pg_json(json).unwrap());
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/plan-index.png"))
                .unwrap();
            let tab = &mut harness.state_mut().tabs[1];
            tab.editor_mut().unwrap().plan = None;
            tab.result = saved;

            let info = crate::db::ServerInfo {
                facts: [
                    ("Versão", "16.4"),
                    ("No ar há", "3 days 04:12:09"),
                    ("Tamanho do banco", "1873 MB"),
                    ("Conexões", "7 de 100"),
                    ("Acerto do cache", "99.21%"),
                ]
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
                largest: QueryResult {
                    columns: ["tabela", "total", "dados", "indices", "linhas_est"]
                        .map(String::from)
                        .to_vec(),
                    rows: [
                        ["public.pedidos", "1012 MB", "652 MB", "360 MB", "5000000"],
                        ["public.clientes", "186 MB", "141 MB", "45 MB", "1000000"],
                        ["public.eventos", "171 MB", "140 MB", "31 MB", "300000"],
                    ]
                    .iter()
                    .map(|r| r.iter().map(|v| Some(v.to_string())).collect())
                    .collect(),
                    ..Default::default()
                },
                indexes: QueryResult {
                    columns: ["tabela", "indice", "usos", "tamanho", "apoia_chave_estrangeira"]
                        .map(String::from)
                        .to_vec(),
                    rows: [
                        ["public.pedidos", "pedidos_criado_idx", "0", "107 MB", ""],
                        ["public.pedidos", "pedidos_cliente_idx", "1", "107 MB", "sim"],
                    ]
                    .iter()
                    .map(|r| r.iter().map(|v| Some(v.to_string())).collect())
                    .collect(),
                    ..Default::default()
                },
            };
            let st = crate::app::ActivityState {
                view: crate::db::ActivityView::Server,
                server: Some(Ok(info)),
                auto_refresh: false,
                last_refresh: None,
                confirm_kill: None,
                kill_error: None,
            };
            let state = harness.state_mut();
            state
                .tabs
                .push(Tab::new(9, "Atividade".into(), TabKind::Activity(Box::new(st))));
            state.active = 2;
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/server.png"))
                .unwrap();
            let state = harness.state_mut();
            state.tabs.pop();
            state.active = 1;
        }

        // Last column numeric (right-aligned): its values must not run past the grid's edge.
        {
            let state = harness.state_mut();
            state.active = 1;
            let tab = &mut state.tabs[1];
            let saved = tab.result.take();
            tab.result = Some(QueryResult {
                columns: vec!["id".into(), "nome".into(), "email".into(), "saldo".into()],
                rows: (1..=20)
                    .map(|i| {
                        vec![
                            Some((i * 37).to_string()),
                            Some(format!("Cliente {}", i * 37)),
                            Some(format!("cliente{}@exemplo.com", i * 37)),
                            Some(format!("{}.{:02}", 10_000 + i * 1_234, i)),
                        ]
                    })
                    .collect(),
                ..Default::default()
            });
            harness.run();
            let img = harness.render().unwrap();
            img.save(format!("{dir}/last-numeric.png")).unwrap();
            let crop = image::imageops::crop_imm(&img, 760, 280, 520, 200).to_image();
            image::imageops::resize(&crop, 1040, 400, image::imageops::FilterType::Nearest)
                .save(format!("{dir}/last-numeric-zoom.png"))
                .unwrap();
            // With the detail panel open too.
            harness.state_mut().tabs[1].selected = Some((2, 3));
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/last-numeric-detail.png"))
                .unwrap();
            let tab = &mut harness.state_mut().tabs[1];
            tab.selected = None;
            tab.result = saved;
        }

        // Schema columns for the hover card, the column search and the diagram.
        {
            let col = |schema: &str, table: &str, column: &str, ty: &str| crate::db::SchemaColumn {
                schema: schema.into(),
                table: table.into(),
                column: column.into(),
                data_type: ty.into(),
                pk: column == "id",
            };
            let s = harness.state_mut().session.as_mut().unwrap();
            s.schema_cols = vec![
                col("public", "clientes", "id", "integer"),
                col("public", "clientes", "nome", "text"),
                col("public", "clientes", "email", "text"),
                col("public", "clientes", "ativo", "boolean"),
                col("public", "clientes", "saldo", "numeric"),
                col("public", "clientes", "criado_em", "timestamp"),
                col("public", "pedidos", "id", "bigint"),
                col("public", "pedidos", "cliente_id", "integer"),
                col("public", "pedidos", "produto_id", "integer"),
                col("public", "pedidos", "status", "text"),
                col("public", "pedidos", "total", "numeric"),
                col("public", "produtos", "id", "integer"),
                col("public", "produtos", "nome", "text"),
                col("public", "produtos", "preco", "numeric"),
                col("public", "produtos", "categoria_id", "integer"),
            ];
        }
        // Hover card over a table name in the editor.
        {
            let state = harness.state_mut();
            state.active = 1;
            if let Some(text) = state.tabs[1].editor_mut().map(|ed| &mut ed.text) {
                *text = "SELECT *\nFROM clientes\nWHERE ativo;".into();
            }
            harness.run();
            harness.hover_at(egui::pos2(316.0, 104.0));
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/hover-table.png"))
                .unwrap();
            harness.hover_at(egui::pos2(900.0, 700.0));
        }
        // ER diagram of "public".
        {
            let fk = |t: &str, c: &str, rt: &str| crate::db::FkEdge {
                schema: "public".into(),
                table: t.into(),
                column: c.into(),
                ref_schema: "public".into(),
                ref_table: rt.into(),
                ref_column: "id".into(),
            };
            let state = harness.state_mut();
            state.open_diagram(Some("public"));
            let (tables, cols) = {
                let s = state.session.as_ref().unwrap();
                (s.tables.clone(), s.schema_cols.clone())
            };
            // As if the foreign keys had arrived.
            state.tabs.last_mut().unwrap().end_job();
            if let Some(TabKind::Diagram(d)) = state.tabs.last_mut().map(|t| &mut t.kind) {
                d.edges = Some(Ok(vec![
                    fk("pedidos", "cliente_id", "clientes"),
                    fk("pedidos", "produto_id", "produtos"),
                ]));
                d.build(&tables, &cols);
            }
            harness.run();
            harness.hover_at(egui::pos2(500.0, 300.0));
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/diagram.png"))
                .unwrap();
            let state = harness.state_mut();
            state.tabs.pop();
            state.active = 1;
        }
        // "Buscar valor" with a finished search.
        {
            let state = harness.state_mut();
            state.open_search();
            if let Some(TabKind::Search(st)) = state.tabs.last_mut().map(|t| &mut t.kind) {
                st.term = "cliente42@exemplo.com".into();
                st.focus = false;
                st.total = 214;
                st.done = 214;
                st.skipped = 1;
                st.hits = vec![
                    crate::search::SearchHit {
                        schema: "public".into(),
                        table: "clientes".into(),
                        columns: vec!["email".into()],
                        rows: 1,
                        filter: "(CAST(\"email\" AS TEXT) ILIKE '%cliente42@exemplo.com%')".into(),
                    },
                    crate::search::SearchHit {
                        schema: "public".into(),
                        table: "eventos".into(),
                        columns: vec!["payload".into(), "descricao".into()],
                        rows: 5,
                        filter: String::new(),
                    },
                ];
            }
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/search.png"))
                .unwrap();
            let state = harness.state_mut();
            state.tabs.pop();
            state.active = 1;
        }

        // Full history, the preferences dialog, then the light theme.
        {
            let state = harness.state_mut();
            let now = crate::history::now();
            for (i, (sql, ok)) in [
                (
                    "SELECT * FROM clientes WHERE uf = 'SP' ORDER BY id LIMIT 50",
                    true,
                ),
                ("UPDATE pedidos SET status = 'pago' WHERE id = 42", true),
                (
                    "SELECT c.uf, count(*) FROM pedidos p JOIN clientes c ON c.id = p.cliente_id GROUP BY 1",
                    true,
                ),
                ("SELECT * FROM produtos", false),
            ]
            .iter()
            .enumerate()
            {
                state.history.push(crate::history::Entry {
                    conn: "dev".into(),
                    sql: sql.to_string(),
                    at: now - 600 * (i as u64 + 1),
                    ok: *ok,
                    ms: 12 * (i as u64 + 1),
                });
            }
            state.open_history();
            let sel = state.history.last().map(|e| (e.conn.clone(), e.sql.clone()));
            if let Some(TabKind::History(h)) = state.tabs.last_mut().map(|t| &mut t.kind) {
                h.selected = sel;
                h.focus = false;
            }
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/history.png"))
                .unwrap();
            let state = harness.state_mut();
            state.tabs.pop();
            state.active = 0;
            let ctx = harness.ctx.clone();
            let light = crate::settings::Settings {
                light: true,
                ..harness.state().settings.clone()
            };
            harness.state_mut().apply_settings(&ctx, light);
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/light.png"))
                .unwrap();
            harness.state_mut().active = 1;
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/light-sql.png"))
                .unwrap();
            harness.state_mut().palette = Some(crate::app::Palette {
                query: "zoom".into(),
                ..Default::default()
            });
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/palette-zoom.png"))
                .unwrap();
            harness.state_mut().palette = None;
            let dark = crate::settings::Settings {
                light: false,
                ..harness.state().settings.clone()
            };
            harness.state_mut().apply_settings(&ctx, dark);
            harness.state_mut().active = 1;
        }

        // Sidebar with functions, triggers and sequences; then "definir valor" on a selection.
        {
            let obj = |name: &str, kind: crate::db::ObjKind, table: &str| crate::db::DbObject {
                schema: "public".into(),
                name: name.into(),
                kind,
                args: String::new(),
                table: table.into(),
                id: String::new(),
            };
            let state = harness.state_mut();
            state.session.as_mut().unwrap().objects = vec![
                obj("atualiza_saldo", crate::db::ObjKind::Function, ""),
                obj("calcula_frete", crate::db::ObjKind::Function, ""),
                obj("fecha_mes", crate::db::ObjKind::Procedure, ""),
                obj("pedidos_auditoria", crate::db::ObjKind::Trigger, "pedidos"),
                obj("pedidos_id_seq", crate::db::ObjKind::Sequence, ""),
            ];
            state.active = 0;
            let tab = &mut state.tabs[0];
            tab.anchor = Some((1, 2));
            tab.selected = Some((4, 2));
            tab.fill_prompt = Some("pendente".into());
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/objects-fill.png"))
                .unwrap();
            let tab = &mut harness.state_mut().tabs[0];
            tab.fill_prompt = None;
            tab.anchor = None;
            tab.selected = Some((2, 2));
        }

        // Autocomplete: type a partial word and put the cursor after it.
        let editor = egui::Id::new(("sql", 2u64));
        if let Some(text) = harness.state_mut().tabs[1].editor_mut().map(|ed| &mut ed.text) {
            *text = "SELECT no".into();
        }
        harness.ctx.memory_mut(|m| m.request_focus(editor));
        harness.run();
        let mut st = egui::TextEdit::load_state(&harness.ctx, editor).unwrap_or_default();
        st.cursor
            .set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(9))));
        st.store(&harness.ctx, editor);
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/complete.png"))
            .unwrap();

        harness.state_mut().palette = Some(crate::app::Palette {
            query: "cl".into(),
            ..Default::default()
        });
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/palette.png"))
            .unwrap();
        harness.state_mut().palette = None;

        harness.state_mut().active = 0;
        if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
            ts.bool_pick = Some((2, 3));
        }
        harness.run();
        let img = harness.render().unwrap();
        img.save(format!("{dir}/boolpick.png")).unwrap();
        let crop = image::imageops::crop_imm(&img, 600, 120, 360, 160).to_image();
        image::imageops::resize(&crop, 1080, 480, image::imageops::FilterType::Nearest)
            .save(format!("{dir}/boolpick-zoom.png"))
            .unwrap();
        if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
            ts.bool_pick = None;
        }
        // Range selection with its totals.
        {
            let tab = &mut harness.state_mut().tabs[0];
            tab.anchor = Some((1, 3));
            tab.selected = Some((6, 4));
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/range.png"))
                .unwrap();
            let tab = &mut harness.state_mut().tabs[0];
            tab.anchor = None;
            tab.selected = Some((2, 2));
        }
        // Pending changes: two edits and a deletion, then the save summary.
        {
            let state = harness.state_mut();
            let s = state.session.as_ref().unwrap();
            let tab = &mut state.tabs[0];
            for ev in [
                crate::app::GridEvent::Commit(1, 1, Some("Cliente Dois".into())),
                crate::app::GridEvent::Commit(1, 4, Some("999.90".into())),
                crate::app::GridEvent::DeleteRow(4),
            ] {
                crate::app::handle_grid_event(tab, s, ev, true);
            }
        }
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/pending.png"))
            .unwrap();
        if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
            ts.confirm_save = true;
        }
        harness.run();
        harness.render().unwrap().save(format!("{dir}/save.png")).unwrap();
        if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
            ts.confirm_save = false;
            ts.changes.clear();
        }

        harness.state_mut().active = 1;
        harness.state_mut().fav_prompt = Some(crate::app::FavPrompt {
            name: "Clientes com mais saldo".into(),
            sql: "SELECT id, nome, saldo\nFROM public.clientes\nWHERE ativo\nORDER BY saldo DESC;".into(),
            shared: false,
        });
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/favorite.png"))
            .unwrap();
        harness.state_mut().fav_prompt = None;
        harness.state_mut().tabs[1].confirm_run = Some("DELETE FROM clientes; DROP TABLE pedidos".into());
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/confirm-run.png"))
            .unwrap();
        harness.state_mut().tabs[1].confirm_run = None;

        harness.state_mut().master = Some(crate::app::MasterPrompt {
            mode: crate::app::MasterMode::Create,
            password: "abc".into(),
            confirm: String::new(),
            error: None,
            then: crate::app::AfterMaster::Nothing,
            working: None,
        });
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/master-create.png"))
            .unwrap();
        harness.state_mut().master = Some(crate::app::MasterPrompt {
            mode: crate::app::MasterMode::Unlock,
            password: String::new(),
            confirm: String::new(),
            error: Some("Senha mestra incorreta".into()),
            then: crate::app::AfterMaster::Nothing,
            working: None,
        });
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/master-unlock.png"))
            .unwrap();
        harness.state_mut().master = None;

        // Switched to another schema: it moves up, opens and is marked; the others fold.
        harness.state_mut().session.as_mut().unwrap().current_schema = "auth".into();
        harness.state_mut().schema_focus = Some("auth".into());
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/schema-switch.png"))
            .unwrap();
        harness.state_mut().session.as_mut().unwrap().current_schema = "public".into();
        harness.state_mut().schema_focus = Some("public".into());
        harness.run();

        // Table action (drop) with its confirmation, then a script file that stopped on an error.
        harness.state_mut().table_action = Some(crate::app::TableActionPrompt {
            schema: "public".into(),
            table: "clientes".into(),
            action: crate::sql::TableAction::Drop,
            input: "clien".into(),
        });
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/table-action.png"))
            .unwrap();
        harness.state_mut().table_action = None;
        {
            let file = std::env::temp_dir().join("migracao_2026_10.sql");
            std::fs::write(&file, "x".repeat(48_000)).unwrap();
            harness.state_mut().open_script(file);
            let app = harness.state_mut();
            let tab = app.tabs.last_mut().unwrap();
            if let TabKind::Script(st) = &mut tab.kind {
                st.progress = crate::worker::ScriptProgress {
                    statements: 212,
                    copied: 0,
                    skipped: 1,
                    bytes: 31_000,
                    line: 418,
                };
                st.took = Some(std::time::Duration::from_millis(2_340));
                st.outcome = Some(Some(crate::worker::ScriptError {
                    message: "db error: ERROR: column \"telefone\" of relation \"clientes\" already exists"
                        .into(),
                    line: 418,
                    statement: "ALTER TABLE clientes ADD COLUMN telefone text".into(),
                    cancelled: false,
                }));
            }
        }
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/script.png"))
            .unwrap();

        harness.state_mut().tabs.clear();
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/connected.png"))
            .unwrap();

        harness.state_mut().session = None;
        let mut my = ConnConfig::new(Kind::MySql);
        my.name = "loja-producao".into();
        my.host = "db.exemplo.com".into();
        my.database = "loja".into();
        harness.state_mut().saved.push(my);
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/welcome.png"))
            .unwrap();

        let mut remote = ConnConfig::new(Kind::MySql);
        remote.host = "db.exemplo.com".into();
        remote.read_only = true;
        remote.color = Some(crate::db::Tag::Red);
        remote.timeout_secs = 30;
        remote.ssh = Some(crate::db::SshConfig {
            host: "bastion.exemplo.com".into(),
            user: "deploy".into(),
            ..Default::default()
        });
        harness.state_mut().form = Some(crate::app::ConnForm {
            editing: None,
            cfg: remote,
        });
        harness.run();
        harness.render().unwrap().save(format!("{dir}/form.png")).unwrap();
    }
}

#[cfg(test)]
mod layout_tests {
    use eframe::egui;
    use egui_kittest::Harness;

    use super::screenshots::fake_app;

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
    #[ignore]
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
