mod complete;
mod detail;
mod form;
mod grid;
mod palette;
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
    }
}

impl App {
    /// App-wide keys. Consumed here so text fields don't also react to them.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
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

    fn fake_app(ctx: &egui::Context) -> App {
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
            harness.state_mut().tabs.pop();
            harness.state_mut().active = 0;
        }
        // 3x zoom on the grid header's left corner, to check pixel-level alignment.
        let crop = image::imageops::crop_imm(&img, 236, 64, 420, 80).to_image();
        image::imageops::resize(&crop, 1260, 240, image::imageops::FilterType::Nearest)
            .save(format!("{dir}/header-zoom.png"))
            .unwrap();
        let crop = image::imageops::crop_imm(&img, 236, 32, 1044, 48).to_image();
        image::imageops::resize(&crop, 2088, 96, image::imageops::FilterType::Nearest)
            .save(format!("{dir}/toolbar-zoom.png"))
            .unwrap();

        harness.state_mut().active = 1;
        harness.run();
        harness.render().unwrap().save(format!("{dir}/sql.png")).unwrap();

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
