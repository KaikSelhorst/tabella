mod complete;
mod design;
mod detail;
mod find;
mod form;
mod grid;
mod help;
mod import_form;
mod palette;
mod plan_view;
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
        let mut sql_tab = Tab::new(2, "SQL 1".into(), TabKind::Sql { text: sql.into() });
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
                auto_refresh: false,
                last_refresh: None,
                confirm_kill: None,
                kill_error: None,
            };
            let mut tab = Tab::new(4, "Atividade".into(), TabKind::Activity(st));
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
            tab.plan = Some(crate::plan::from_pg_json(json).unwrap());
            harness.run();
            harness.render().unwrap().save(format!("{dir}/plan.png")).unwrap();
            harness.state_mut().tabs[1].plan = None;
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
            if let TabKind::Sql { text } = &mut tab.kind {
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
            if let TabKind::Sql { text } = &mut tab.kind {
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
            if let TabKind::Sql { text } = &mut state.tabs[1].kind {
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
            tab.param_values.insert("id".into(), "42".into());
            tab.params = Some(crate::app::ParamPrompt {
                sql: String::new(),
                names: vec!["id".into(), "cidade".into()],
                explain: None,
                focused: true,
            });
            harness.run();
            harness
                .render()
                .unwrap()
                .save(format!("{dir}/params.png"))
                .unwrap();
            let tab = &mut harness.state_mut().tabs[1];
            tab.params = None;
            tab.result = saved;
            tab.selected = None;
            if let TabKind::Sql { text } = &mut tab.kind {
                *text =
                    "SELECT id, nome, saldo\nFROM public.clientes\nWHERE ativo\nORDER BY saldo DESC;".into();
            }
        }

        // Autocomplete: type a partial word and put the cursor after it.
        let editor = egui::Id::new(("sql", 2u64));
        if let TabKind::Sql { text } = &mut harness.state_mut().tabs[1].kind {
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
