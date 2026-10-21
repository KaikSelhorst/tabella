use std::time::Duration;

use eframe::egui;
use egui_kittest::Harness;

use crate::app::{App, Tab, TabKind, TableState};
use crate::db::{ColumnInfo, ConnConfig, Kind, QueryResult, TableRef};

pub fn fake_app(ctx: &egui::Context) -> App {
    // Keep the real config untouched: App::new reads (and may migrate) it.
    let dir = std::env::temp_dir().join("tabella-screenshots-config");
    crate::store::use_dir_in_this_thread(dir.clone());
    // Widths dragged in an earlier run would change how the screens look.
    let _ = std::fs::remove_file(dir.join("column_widths.json"));
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
        table("public", "customers", false),
        table("public", "orders", false),
        table("public", "products", false),
        table("public", "monthly_sales", true),
        table("auth", "sessions", false),
        table("auth", "users", false),
    ];
    session.columns = ["id", "name", "email", "active", "balance", "created_at"]
        .iter()
        .map(|c| ("customers".to_string(), c.to_string()))
        .chain([("orders".to_string(), "customer_id".to_string())])
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
        entry("SELECT count(*) FROM customers WHERE active", 7200),
        entry("SELECT * FROM orders WHERE customer_id = 3", 600),
    ];
    let col = |name: &str, ty: &str, pk| ColumnInfo {
        enum_values: Vec::new(),
        name: name.into(),
        data_type: ty.into(),
        pk,
        nullable: !pk,
        has_default: false,
        references: None,
    };
    let cols = vec![
        col("id", "integer", true),
        col("name", "text", false),
        col("email", "text", false),
        col("active", "boolean", false),
        col("balance", "numeric", false),
        col("created_at", "timestamp without time zone", false),
    ];
    let rows = (1..=40)
        .map(|i| {
            vec![
                Some(i.to_string()),
                Some(format!("Customer {i}")),
                Some(format!("customer{i}@example.com")),
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
        timings: Vec::new(),
    };
    let mut ts = TableState::new("public", "customers", Some(40));
    ts.cols = cols;
    ts.order = vec![("id".into(), true)];
    ts.total = Some(40);
    ts.counted_filter = Some(String::new());
    // Far from `App::next_id`, so tabs the screens open never share an id with these.
    let mut table_tab = Tab::new(1001, "customers".into(), TabKind::Table(Box::new(ts)));
    table_tab.result = Some(result.clone());
    table_tab.selected = Some((2, 2));
    let sql = "SELECT id, name, balance\nFROM public.customers\nWHERE active\nORDER BY balance DESC;";
    let mut sql_tab = Tab::new(
        1002,
        "SQL 1".into(),
        TabKind::Sql(Box::new(crate::app::Editor::new(sql.into()))),
    );
    sql_tab.result = Some(result);
    app.tabs = vec![table_tab, sql_tab];
    app.active = 0;
    app
}

#[test]
fn dragging_a_tab_to_the_other_strip_moves_it() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.state_mut().split_tab(1);
    harness.run();
    assert_eq!(
        harness.state().tabs.iter().map(|t| t.right).collect::<Vec<_>>(),
        [false, false, true]
    );
    let from = egui::pos2(290.0, 17.0);
    let to = egui::pos2(1000.0, 17.0);
    harness.hover_at(from);
    harness.step();
    harness.drag_at(from);
    harness.step();
    for i in 1..=10 {
        harness.hover_at(from + (to - from) * (i as f32 / 10.0));
        harness.step();
    }
    // Not `drop_at`: it also removes the cursor, which a real mouse doesn't do on release.
    harness.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.run();
    assert_eq!(
        harness.state().tabs.iter().map(|t| t.right).collect::<Vec<_>>(),
        [false, true, true],
        "it lands at the end of the other pane, after the tab already there"
    );
}

#[test]
fn dragging_a_tab_along_its_strip_reorders_it() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.run();
    let titles =
        |h: &Harness<crate::app::App>| h.state().tabs.iter().map(|t| t.title.clone()).collect::<Vec<_>>();
    let before = titles(&harness);
    assert_eq!(before.len(), 2);
    let from = egui::pos2(290.0, 17.0);
    let to = egui::pos2(1100.0, 17.0);
    harness.hover_at(from);
    harness.step();
    harness.drag_at(from);
    harness.step();
    for i in 1..=10 {
        harness.hover_at(from + (to - from) * (i as f32 / 10.0));
        harness.step();
    }
    harness.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.run();
    assert_eq!(titles(&harness), [before[1].clone(), before[0].clone()]);
    assert_eq!(harness.state().active, 1, "the moved tab is the active one");
}

#[test]
fn dragging_a_tab_to_the_right_edge_splits_the_view() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.run();
    assert!(harness.state().split.is_none());
    let from = egui::pos2(290.0, 17.0);
    let to = egui::pos2(1200.0, 400.0);
    harness.hover_at(from);
    harness.step();
    harness.drag_at(from);
    harness.step();
    for i in 1..=10 {
        harness.hover_at(from + (to - from) * (i as f32 / 10.0));
        harness.step();
    }
    harness.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.run();
    assert!(harness.state().split.is_some(), "the split opened");
    assert_eq!(
        harness.state().tabs.iter().map(|t| t.right).collect::<Vec<_>>(),
        [true, false],
        "the dragged tab is alone on the right"
    );
}

#[test]
fn dragging_a_column_edge_is_remembered_for_the_table() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.run();
    harness.state_mut().tabs[0].col_widths.clear();
    let before = harness.state().col_widths_for("customers");
    assert!(before.is_empty());
    // The "name" column's right edge sits at about x = 543 in this layout (see table.png).
    let (edge, y) = (egui::pos2(543.0, 96.0), 96.0);
    harness.hover_at(edge);
    harness.step();
    harness.drag_at(edge);
    harness.step();
    for i in 1..=8 {
        harness.hover_at(egui::pos2(543.0 + 12.0 * i as f32, y));
        harness.step();
    }
    // Still held: nothing is remembered while dragging.
    assert!(harness.state().col_widths_for("customers").is_empty());
    harness.event(egui::Event::PointerButton {
        pos: egui::pos2(543.0 + 96.0, y),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.run();
    let after = harness.state().col_widths_for("customers");
    assert!(after["name"] > 200.0, "{after:?}");
    assert_eq!(harness.state().tabs[0].col_widths["name"], after["name"]);
}

/// `TABELLA_SHOTS=<dir> cargo test copy_menu_shot -- --ignored`: the Export menu and the copied note.
#[test]
#[ignore]
fn copy_menu_shot() {
    let dir = std::env::var("TABELLA_SHOTS").unwrap_or_else(|_| "target/shots".into());
    std::fs::create_dir_all(&dir).unwrap();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .with_max_steps(8)
        .wgpu()
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.run();
    let at = egui::pos2(1075.0, 57.0);
    harness.hover_at(at);
    harness.step();
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
    }
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/copy-menu.png"))
        .unwrap();
}

/// `TABELLA_SHOTS=<dir> cargo test bulk_dialog_shot -- --ignored`: "Set with an expression".
#[test]
#[ignore]
fn bulk_dialog_shot() {
    let dir = std::env::var("TABELLA_SHOTS").unwrap_or_else(|_| "target/shots".into());
    std::fs::create_dir_all(&dir).unwrap();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .with_max_steps(8)
        .wgpu()
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.run();
    let state = harness.state_mut();
    state.active = 0;
    let tab = &mut state.tabs[0];
    tab.selected = Some((2, 4));
    tab.anchor = Some((0, 4));
    tab.bulk_prompt = Some(crate::app::BulkPrompt {
        column: "balance".into(),
        expression: "balance * 1.1".into(),
        selected_only: true,
    });
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/bulk-dialog.png"))
        .unwrap();
}

/// `TABELLA_SHOTS=<dir> cargo test plan_compare_shot -- --ignored`: a plan against the pinned one.
#[test]
#[ignore]
fn plan_compare_shot() {
    let dir = std::env::var("TABELLA_SHOTS").unwrap_or_else(|_| "target/shots".into());
    std::fs::create_dir_all(&dir).unwrap();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .with_max_steps(8)
        .wgpu()
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.run();
    let before = r#"[{"Plan": {"Node Type": "Seq Scan", "Relation Name": "orders", "Total Cost": 91000, "Plan Rows": 5000000, "Actual Total Time": 610.0, "Actual Rows": 41, "Actual Loops": 1, "Filter": "(customer_id = 7)"}, "Planning Time": 0.3, "Execution Time": 610.4}]"#;
    let after = r#"[{"Plan": {"Node Type": "Index Scan", "Index Name": "idx_orders_customer_id", "Relation Name": "orders", "Total Cost": 12.4, "Plan Rows": 40, "Actual Total Time": 0.09, "Actual Rows": 41, "Actual Loops": 1, "Index Cond": "(customer_id = 7)"}, "Planning Time": 0.4, "Execution Time": 0.2}]"#;
    let state = harness.state_mut();
    state.active = 1;
    let ed = state.tabs[1].editor_mut().unwrap();
    ed.pinned_plan = Some(crate::plan::from_pg_json(before).unwrap());
    ed.plan = Some(crate::plan::from_pg_json(after).unwrap());
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/plan-compare.png"))
        .unwrap();
}

/// `TABELLA_SHOTS=<dir> cargo test sort_pin_shot -- --ignored`: two sort columns and a pinned tab.
#[test]
#[ignore]
fn sort_pin_shot() {
    let dir = std::env::var("TABELLA_SHOTS").unwrap_or_else(|_| "target/shots".into());
    std::fs::create_dir_all(&dir).unwrap();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .with_max_steps(8)
        .wgpu()
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.run();
    let state = harness.state_mut();
    state.active = 0;
    if let TabKind::Table(ts) = &mut state.tabs[0].kind {
        ts.order = vec![("name".into(), true), ("email".into(), false)];
    }
    state.set_pinned(1, true);
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/sort-pin.png"))
        .unwrap();
}

/// `TABELLA_SHOTS=<dir> cargo test index_advice_shot -- --ignored`: the "Suggest index" dialog.
#[test]
#[ignore]
fn index_advice_shot() {
    let dir = std::env::var("TABELLA_SHOTS").unwrap_or_else(|_| "target/shots".into());
    std::fs::create_dir_all(&dir).unwrap();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .with_max_steps(8)
        .wgpu()
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.run();
    let q = "SELECT * FROM orders WHERE customer_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT 20";
    let res = QueryResult {
        columns: ["calls", "total_ms", "mean_ms", "row_count", "query"]
            .map(String::from)
            .to_vec(),
        rows: vec![
            ["8120", "91234.5", "11.23", "162400", q]
                .map(|v| Some(v.to_string()))
                .to_vec(),
        ],
        ..Default::default()
    };
    let plans = crate::advisor::parse(q).unwrap();
    let advice = crate::advisor::advise(crate::db::Kind::Postgres, "public", &plans, |_, _| {
        Some(crate::db::TableStructure {
            columns: ["id", "customer_id", "status", "created_at"]
                .map(|n| crate::db::StructColumn {
                    name: n.into(),
                    data_type: "text".into(),
                    nullable: true,
                    default: None,
                    pk: false,
                })
                .to_vec(),
            indexes: vec![(
                "orders_customer_idx".into(),
                "CREATE INDEX orders_customer_idx ON public.orders USING btree (customer_id)".into(),
            )],
            ..Default::default()
        })
    });
    let st = crate::app::ActivityState {
        view: crate::db::ActivityView::Slow,
        server: None,
        auto_refresh: false,
        last_refresh: None,
        confirm_kill: None,
        kill_error: None,
        advice: Some(advice),
    };
    let mut tab = Tab::new(4, "Activity".into(), TabKind::Activity(Box::new(st)));
    tab.result = Some(res);
    tab.selected = Some((0, 4));
    let state = harness.state_mut();
    state.tabs.push(tab);
    state.active = 2;
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/index-advice.png"))
        .unwrap();
}

/// `TABELLA_SHOTS=<dir> cargo test tab_drag_shot -- --ignored`: a tab in the middle of a drag.
#[test]
#[ignore]
fn tab_drag_shot() {
    let dir = std::env::var("TABELLA_SHOTS").unwrap_or_else(|_| "target/shots".into());
    std::fs::create_dir_all(&dir).unwrap();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .with_max_steps(8)
        .wgpu()
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.run();
    let from = egui::pos2(290.0, 17.0);
    harness.hover_at(from);
    harness.step();
    harness.drag_at(from);
    harness.step();
    for i in 1..=6 {
        harness.hover_at(from + egui::vec2(40.0 * i as f32, 8.0));
        harness.step();
    }
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/tab-drag.png"))
        .unwrap();
}

/// `TABELLA_SHOTS=<dir> cargo test detail_panel_shot -- --ignored`: awkward values in the row panel.
#[test]
#[ignore]
fn detail_panel_shot() {
    let dir = std::env::var("TABELLA_SHOTS").unwrap_or_else(|_| "target/shots".into());
    std::fs::create_dir_all(&dir).unwrap();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .with_max_steps(8)
        .wgpu()
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.run();
    let state = harness.state_mut();
    state.active = 0;
    let long = "https://cdn.example.com/assets/very/long/path/without/spaces/image_0123456789abcdef0123456789abcdef.png?token=abcdefghijklmnopqrstuvwxyz0123456789";
    state.tabs[0].result = Some(QueryResult {
        columns: vec![
            "id".into(),
            "name".into(),
            "email".into(),
            "url".into(),
            "doc".into(),
        ],
        rows: vec![vec![
            Some("1".into()),
            Some("Customer with a rather long display name that keeps going".into()),
            Some("customer1@example.com".into()),
            Some(long.into()),
            Some(format!(
                r#"{{"key_with_long_name_here":"{long}","items":[1,2,3]}}"#
            )),
        ]],
        ..Default::default()
    });
    state.tabs[0].selected = Some((0, 0));
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/detail-long.png"))
        .unwrap();
    // Read-only (a view or no primary key) takes another path through the panel.
    harness.state_mut().session.as_mut().unwrap().cfg.read_only = true;
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/detail-long-ro.png"))
        .unwrap();
}

#[test]
#[ignore]
fn screenshots() {
    let dir = std::env::var("TABELLA_SHOTS").unwrap_or_else(|_| "target/shots".into());
    std::fs::create_dir_all(&dir).unwrap();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        // A screen that never stops redrawing still fails; a modal opening while the grid's
        // row highlight follows shifted rows takes a few frames to settle.
        .with_max_steps(8)
        .wgpu()
        .build_eframe(|cc| {
            super::theme::apply(&cc.egui_ctx);
            fake_app(&cc.egui_ctx)
        });
    harness.run();
    let img = harness.render().unwrap();
    img.save(format!("{dir}/table.png")).unwrap();

    harness.state_mut().split_tab(1);
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/split.png"))
        .unwrap();
    // The split copied the SQL tab: drop the copy so the later screens keep their tab order.
    harness.state_mut().close_split();
    harness.state_mut().tabs.pop();
    harness.state_mut().active = 0;
    harness.run();

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
        let csv = std::env::temp_dir().join("tabella-shot-customers.csv");
        std::fs::write(
            &csv,
            "Name;Email;Active;Balance;Notes\nMary Smith;mary@example.com;yes;1520,30;longtime customer\nJohn Jones;john@example.com;no;;\nAnna Parker;anna@example.com;yes;88,00;referral\n",
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
        let xlsx = crate::import::tests::xlsx("shot");
        crate::app::open_import(&mut harness.state_mut().tabs[0], Some(xlsx.display().to_string()));
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/import-xlsx.png"))
            .unwrap();
        if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
            ts.import = None;
        }
        let _ = std::fs::remove_file(xlsx);
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
                col("id", "integer", false, Some("nextval('customers_id_seq'::regclass)"), true),
                col("name", "text", true, None, false),
                col("email", "text", true, None, false),
                col("active", "boolean", true, Some("true"), false),
                col("balance", "numeric(12,2)", true, None, false),
                col("created_at", "timestamp without time zone", true, Some("now()"), false),
            ],
            indexes: vec![
                ("customers_pkey".into(), "CREATE UNIQUE INDEX customers_pkey ON public.customers USING btree (id)".into()),
                ("customers_email_idx".into(), "CREATE INDEX customers_email_idx ON public.customers USING btree (email)".into()),
            ],
            foreign_keys: vec![],
            ddl: "CREATE TABLE \"public\".\"customers\" (\n    \"id\" integer NOT NULL DEFAULT nextval('customers_id_seq'::regclass),\n    \"name\" text,\n    \"email\" text,\n    \"active\" boolean DEFAULT true,\n    \"balance\" numeric(12,2),\n    \"created_at\" timestamp without time zone DEFAULT now(),\n    CONSTRAINT \"customers_pkey\" PRIMARY KEY (id)\n);\nCREATE INDEX customers_email_idx ON public.customers USING btree (email);".into(),
            ..Default::default()
        };
        let st = crate::app::StructState {
            schema: "public".into(),
            table: "customers".into(),
            data: Some(data),
            focus: Some("email".into()),
            focus_seq: 1,
        };
        let tab = Tab::new(
            3,
            "customers · structure".into(),
            TabKind::Structure(Box::new(st)),
        );
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
        harness.state_mut().open_design("public", "customers", &data);
        if let Some(TabKind::Design(d)) = harness.state_mut().tabs.last_mut().map(|t| &mut t.kind) {
            d.columns[1].name = "full_name".into();
            d.columns[1].nullable = false;
            d.columns[4].dropped = true;
            d.columns.push(crate::design::ColDef {
                name: "state".into(),
                data_type: "char(2)".into(),
                default: "'NY'".into(),
                ..crate::design::ColDef::new()
            });
            d.indexes.push(crate::design::IndexDef {
                existing: false,
                name: String::new(),
                columns: "state, created_at".into(),
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
        harness.state_mut().dialog = Some(crate::app::Dialog::Help);
        harness.run();
        harness.render().unwrap().save(format!("{dir}/help.png")).unwrap();
        harness.state_mut().dialog = None;

        // "Duplicate row": the insert form filled from row 3.
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
            "pid", "username", "db", "state", "duration", "wait", "client", "query",
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
                    "SELECT count(*) FROM orders p JOIN customers c ON c.id = p.customer_id",
                ),
                row(
                    "4188",
                    "idle in transaction",
                    "00:03:12",
                    "UPDATE customers SET balance = 0 WHERE id = 7",
                ),
                row("4203", "idle", "00:12:55", "SELECT 1"),
            ],
            affected: None,
            truncated: false,
            elapsed: Duration::from_millis(2),
            previous: Vec::new(),
            timings: Vec::new(),
        };
        let st = crate::app::ActivityState {
            view: crate::db::ActivityView::Sessions,
            server: None,
            auto_refresh: false,
            last_refresh: None,
            confirm_kill: None,
            kill_error: None,
            advice: None,
        };
        let mut tab = Tab::new(4, "Activity".into(), TabKind::Activity(Box::new(st)));
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
            columns: ["calls", "total_ms", "mean_ms", "row_count", "query"].map(String::from).to_vec(),
            rows: [
                ["1240", "98213.4", "79.20", "1240", "SELECT count(*) FROM orders p JOIN customers c ON c.id = p.customer_id WHERE p.status = $1"],
                ["58201", "41022.8", "0.70", "58201", "SELECT * FROM customers WHERE email = $1"],
                ["12", "8890.1", "740.84", "60000000", "SELECT * FROM events ORDER BY created_at DESC"],
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
            "Hash Cond": "(p.customer_id = c.id)",
            "Plans": [
              {"Node Type": "Seq Scan", "Relation Name": "orders", "Alias": "p", "Total Cost": 91000, "Plan Rows": 5000000, "Actual Total Time": 610.0, "Actual Rows": 5000000, "Actual Loops": 1, "Filter": "(status = 'paid'::text)", "Rows Removed by Filter": 4170000},
              {"Node Type": "Hash", "Total Cost": 3100, "Plan Rows": 900, "Actual Total Time": 21.0, "Actual Rows": 900, "Actual Loops": 1,
                "Plans": [{"Node Type": "Index Scan", "Index Name": "customers_pkey", "Relation Name": "customers", "Alias": "c", "Total Cost": 3000, "Plan Rows": 900, "Actual Total Time": 18.0, "Actual Rows": 900, "Actual Loops": 1, "Index Cond": "(id < 1000)"}]}
            ]}]},
          "Planning Time": 0.4, "Execution Time": 812.5}]"#;
        let tab = &mut harness.state_mut().tabs[1];
        tab.editor_mut().unwrap().plan = Some(crate::plan::from_pg_json(json).unwrap());
        harness.run();
        harness.render().unwrap().save(format!("{dir}/plan.png")).unwrap();
        harness.state_mut().tabs[1].editor_mut().unwrap().plan = None;
    }

    // Wide editor result scrolled sideways: "id" and "name" are pinned and stay at the left.
    {
        let state = harness.state_mut();
        state.active = 1;
        let tab = &mut state.tabs[1];
        let saved = tab.result.take();
        let names: Vec<String> = ["id", "name"]
            .iter()
            .map(|s| s.to_string())
            .chain((1..=14).map(|i| format!("field_{i:02}")))
            .collect();
        tab.result = Some(QueryResult {
            rows: (1..=30)
                .map(|r| {
                    names
                        .iter()
                        .enumerate()
                        .map(|(i, _)| match i {
                            0 => Some(r.to_string()),
                            1 => Some(format!("Customer {r}")),
                            _ => Some(format!("value {r}-{i}")),
                        })
                        .collect()
                })
                .collect(),
            columns: names,
            ..Default::default()
        });
        tab.pinned_cols = vec!["id".into(), "name".into()];
        tab.hidden_cols = vec!["field_03".into()];
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
        let script = "SELECT count(*) FROM customers;\n\nSELECT id, name, balance\nFROM customers\nWHERE active;\n\nSELECT * FROM orders LIMIT 5;";
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
        // Cells saved in that transaction: marked in the grid until it ends.
        let cell = |id: &str, column: &str| crate::app::UncommittedCell {
            schema: "public".into(),
            table: "customers".into(),
            key: vec![Some(id.into())],
            column: column.into(),
        };
        state.session.as_mut().unwrap().uncommitted =
            [cell("2", "name"), cell("4", "balance"), cell("4", "active")]
                .into_iter()
                .collect();
        state.tabs[0].col_stats = Some(crate::app::StatsView {
            column: "city".into(),
            scope: "whole table".into(),
            data: Some(Ok(crate::values::ColumnStats {
                total: 1_000_000,
                nulls: 0,
                distinct: 10,
                min: None,
                max: None,
                top: ["New York", "Los Angeles", "Chicago", "Chicago", "Boston"]
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
        state.session.as_mut().unwrap().uncommitted.clear();
    }

    // Editor: find bar, then the parameters prompt, then a JSON value in the detail panel.
    {
        let state = harness.state_mut();
        state.active = 1;
        if let Some(text) = state.tabs[1].editor_mut().map(|ed| &mut ed.text) {
            *text = "SELECT *\nFROM customers\nWHERE id = :id\n  AND city = :city\n  AND id IN (SELECT customer_id FROM orders);".into();
        }
        let saved = state.tabs[1].result.take();
        state.tabs[1].result = Some(QueryResult {
            columns: vec!["id".into(), "payload".into()],
            rows: vec![vec![
                Some("7".into()),
                Some(r#"{"page": "/product/42", "browser": "firefox", "items": [{"sku": "A-1", "price": 19.9}, {"sku": "B-2", "price": 5}], "coupon": null, "new": true}"#.into()),
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
            names: vec!["id".into(), "city".into()],
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
            *text = "SELECT id, name, balance\nFROM public.customers\nWHERE active\nORDER BY balance DESC;"
                .into();
        }
    }

    // Quick chart (bars, then a line over dates) and the UPDATE preview.
    {
        let state = harness.state_mut();
        state.active = 1;
        let tab = &mut state.tabs[1];
        let saved = tab.result.take();
        let cities = [
            "New York",
            "Los Angeles",
            "Chicago",
            "Chicago",
            "Seattle",
            "Boston",
            "Miami",
            "Denver",
        ];
        tab.result = Some(QueryResult {
            columns: vec!["city".into(), "total".into()],
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
            columns: vec!["day".into(), "orders".into()],
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
            sql: "UPDATE customers SET active = false\nWHERE state = 'NY' AND created_at < '2023-01-01'"
                .into(),
            data: Some(Ok(crate::db::PreviewData {
                count: 18_342,
                sample: QueryResult {
                    columns: vec![
                        "id".into(),
                        "name".into(),
                        "state".into(),
                        "active".into(),
                        "created_at".into(),
                    ],
                    rows: (0..12)
                        .map(|i| {
                            vec![
                                Some((i * 10 + 3).to_string()),
                                Some(format!("Customer {}", i * 10 + 3)),
                                Some("NY".into()),
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
            columns: vec!["id".into(), "name".into(), "balance".into()],
            rows: (1..=12)
                .map(|i| {
                    vec![
                        Some(i.to_string()),
                        Some(format!("Customer {i}")),
                        Some(format!("{}.00", i * 120)),
                    ]
                })
                .collect(),
            ..Default::default()
        });
        let mut src = crate::app::TableState::new("public", "customers", None);
        let col = |name: &str, ty: &str, pk| ColumnInfo {
            enum_values: Vec::new(),
            name: name.into(),
            data_type: ty.into(),
            pk,
            nullable: !pk,
            has_default: false,
            references: None,
        };
        src.cols = vec![
            col("id", "integer", true),
            col("name", "text", false),
            col("balance", "numeric", false),
        ];
        src.changes.push(crate::app::Change::Set {
            key: vec![Some("3".into())],
            col: "name".into(),
            value: Some("Customer Three".into()),
            before: Some("Customer 3".into()),
        });
        tab.editor_mut().unwrap().source = Some(Box::new(src));
        tab.editor_mut().unwrap().source_query =
            "SELECT id, name, balance FROM customers WHERE active".into();
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

        let json = r#"[{"Plan": {"Node Type": "Seq Scan", "Relation Name": "customers", "Alias": "customers", "Total Cost": 21000, "Plan Rows": 31000, "Filter": "((balance > '1000'::numeric) AND (active = true))"}}]"#;
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
                ("Version", "16.4"),
                ("Uptime", "3 days 04:12:09"),
                ("Database size", "1873 MB"),
                ("Connections", "7 of 100"),
                ("Cache hit rate", "99.21%"),
            ]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
            largest: QueryResult {
                columns: ["table_name", "total", "data_size", "index_size", "est_rows"]
                    .map(String::from)
                    .to_vec(),
                rows: [
                    ["public.orders", "1012 MB", "652 MB", "360 MB", "5000000"],
                    ["public.customers", "186 MB", "141 MB", "45 MB", "1000000"],
                    ["public.events", "171 MB", "140 MB", "31 MB", "300000"],
                ]
                .iter()
                .map(|r| r.iter().map(|v| Some(v.to_string())).collect())
                .collect(),
                ..Default::default()
            },
            indexes: QueryResult {
                columns: ["table_name", "index_name", "scans", "size", "backs_foreign_key"]
                    .map(String::from)
                    .to_vec(),
                rows: [
                    ["public.orders", "orders_created_idx", "0", "107 MB", ""],
                    ["public.orders", "orders_customer_idx", "1", "107 MB", "yes"],
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
            advice: None,
        };
        let state = harness.state_mut();
        state
            .tabs
            .push(Tab::new(9, "Activity".into(), TabKind::Activity(Box::new(st))));
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
            columns: vec!["id".into(), "name".into(), "email".into(), "balance".into()],
            rows: (1..=20)
                .map(|i| {
                    vec![
                        Some((i * 37).to_string()),
                        Some(format!("Customer {}", i * 37)),
                        Some(format!("customer{}@example.com", i * 37)),
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
            col("public", "customers", "id", "integer"),
            col("public", "customers", "name", "text"),
            col("public", "customers", "email", "text"),
            col("public", "customers", "active", "boolean"),
            col("public", "customers", "balance", "numeric"),
            col("public", "customers", "created_at", "timestamp"),
            col("public", "orders", "id", "bigint"),
            col("public", "orders", "customer_id", "integer"),
            col("public", "orders", "product_id", "integer"),
            col("public", "orders", "status", "text"),
            col("public", "orders", "total", "numeric"),
            col("public", "products", "id", "integer"),
            col("public", "products", "name", "text"),
            col("public", "products", "price", "numeric"),
            col("public", "products", "category_id", "integer"),
        ];
    }
    // Hover card over a table name in the editor.
    {
        let state = harness.state_mut();
        state.active = 1;
        if let Some(text) = state.tabs[1].editor_mut().map(|ed| &mut ed.text) {
            *text = "SELECT *\nFROM customers\nWHERE active;".into();
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
                fk("orders", "customer_id", "customers"),
                fk("orders", "product_id", "products"),
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
    // "Search value" with a finished search.
    {
        let state = harness.state_mut();
        state.open_search();
        if let Some(TabKind::Search(st)) = state.tabs.last_mut().map(|t| &mut t.kind) {
            st.term = "customer42@example.com".into();
            st.focus = false;
            st.total = 214;
            st.done = 214;
            st.skipped = 1;
            st.hits = vec![
                crate::search::SearchHit {
                    schema: "public".into(),
                    table: "customers".into(),
                    columns: vec!["email".into()],
                    rows: 1,
                    filter: "(CAST(\"email\" AS TEXT) ILIKE '%customer42@example.com%')".into(),
                },
                crate::search::SearchHit {
                    schema: "public".into(),
                    table: "events".into(),
                    columns: vec!["payload".into(), "description".into()],
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
                "SELECT * FROM customers WHERE state = 'NY' ORDER BY id LIMIT 50",
                true,
            ),
            ("UPDATE orders SET status = 'paid' WHERE id = 42", true),
            (
                "SELECT c.state, count(*) FROM orders p JOIN customers c ON c.id = p.customer_id GROUP BY 1",
                true,
            ),
            ("SELECT * FROM products", false),
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
        harness.state_mut().dialog = Some(crate::app::Dialog::Palette(crate::app::Palette {
            query: "zoom".into(),
            ..Default::default()
        }));
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/palette-zoom.png"))
            .unwrap();
        harness.state_mut().dialog = None;
        let dark = crate::settings::Settings {
            light: false,
            ..harness.state().settings.clone()
        };
        harness.state_mut().apply_settings(&ctx, dark);
        harness.state_mut().active = 1;
    }

    // Sidebar with functions, triggers and sequences; then "set value" on a selection.
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
            obj("update_balance", crate::db::ObjKind::Function, ""),
            obj("calc_shipping", crate::db::ObjKind::Function, ""),
            obj("close_month", crate::db::ObjKind::Procedure, ""),
            obj("orders_audit", crate::db::ObjKind::Trigger, "orders"),
            obj("pedidos_id_seq", crate::db::ObjKind::Sequence, ""),
        ];
        state.active = 0;
        let tab = &mut state.tabs[0];
        tab.anchor = Some((1, 2));
        tab.selected = Some((4, 2));
        tab.fill_prompt = Some("pending".into());
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

    harness.state_mut().dialog = Some(crate::app::Dialog::Palette(crate::app::Palette {
        query: "cl".into(),
        ..Default::default()
    }));
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/palette.png"))
        .unwrap();
    harness.state_mut().dialog = None;

    harness.state_mut().active = 0;
    if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
        ts.option_pick = Some((2, 3));
    }
    harness.run();
    let img = harness.render().unwrap();
    img.save(format!("{dir}/boolpick.png")).unwrap();
    let crop = image::imageops::crop_imm(&img, 600, 120, 360, 160).to_image();
    image::imageops::resize(&crop, 1080, 480, image::imageops::FilterType::Nearest)
        .save(format!("{dir}/boolpick-zoom.png"))
        .unwrap();
    if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
        ts.option_pick = None;
    }
    // An ENUM column: the picker with its values, and the combo in the detail panel.
    if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
        ts.cols[1].enum_values = [
            "consumer",
            "small_business",
            "mid_market",
            "enterprise",
            "government",
        ]
        .map(String::from)
        .to_vec();
        ts.option_pick = Some((2, 1));
    }
    harness.state_mut().tabs[0].selected = Some((2, 1));
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/enumpick.png"))
        .unwrap();
    if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
        ts.option_pick = None;
        ts.cols[1].enum_values.clear();
    }
    harness.state_mut().tabs[0].selected = Some((2, 2));
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
            crate::app::GridEvent::Commit(1, 1, Some("Customer Two".into())),
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
    harness.state_mut().dialog = Some(crate::app::Dialog::Favorite(crate::app::FavPrompt {
        name: "Customers with the highest balance".into(),
        sql: "SELECT id, name, balance\nFROM public.customers\nWHERE active\nORDER BY balance DESC;".into(),
        shared: false,
    }));
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/favorite.png"))
        .unwrap();
    harness.state_mut().dialog = None;
    harness.state_mut().tabs[1].confirm_run = Some("DELETE FROM customers; DROP TABLE orders".into());
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
        error: Some("Wrong master password".into()),
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

    // A pinned result against the current one.
    {
        let app = harness.state_mut();
        app.active = 1;
        let tab = &mut app.tabs[1];
        let before = tab.result.clone().unwrap();
        let ed = tab.editor_mut().unwrap();
        ed.set_last_query(
            "SELECT id, name, email, active, balance, created_at FROM customers ORDER BY id".into(),
        );
        ed.pin(&before, None);
        ed.comparing = true;
        let mut now = before.clone();
        now.rows[2][4] = Some("150.00".into());
        now.rows[6][3] = Some("f".into());
        now.rows[9][1] = Some("Customer Ten".into());
        now.rows.remove(4);
        now.rows.push(vec![
            Some("41".into()),
            Some("Customer 41".into()),
            Some("new@example.com".into()),
            Some("t".into()),
            None,
            Some("2026-10-14 09:00:00".into()),
        ]);
        now.elapsed = std::time::Duration::from_millis(4);
        tab.result = Some(now);
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/result-diff.png"))
            .unwrap();
        let tab = &mut harness.state_mut().tabs[1];
        tab.result = Some(before);
        let ed = tab.editor_mut().unwrap();
        ed.pinned = None;
        ed.comparing = false;
    }

    // The foreign-key picker over a big table, searched by name.
    {
        let app = harness.state_mut();
        app.active = 0;
        let row = |k: &str, l: &str| (k.to_string(), Some(l.to_string()));
        app.tabs[0].fk_picker = Some(crate::app::FkPicker {
            target: crate::app::PickFor::Cell(2, 1),
            column: "customer_id".into(),
            fk: crate::db::FkRef {
                schema: "public".into(),
                table: "customers".into(),
                column: "id".into(),
            },
            label: Some("name".into()),
            numeric_key: true,
            local: false,
            search: "mary".into(),
            asked: "mary".into(),
            typed_at: std::time::Instant::now() - std::time::Duration::from_secs(5),
            loading: false,
            options: Some(Ok(vec![
                row("42", "Mary Smith"),
                row("1077", "Mary Ellen Jones"),
                row("23514", "Anna Mary Parker"),
                row("90211", "Marianne Clark"),
            ])),
            current: Some("1077".into()),
            focused: true,
        });
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/fk-picker.png"))
            .unwrap();
        harness.state_mut().tabs[0].fk_picker = None;
    }

    // The big value editor with a JSON document.
    {
        let app = harness.state_mut();
        app.active = 0;
        let raw = r#"{"customer":{"id":3,"name":"Customer 3","vip":true},"items":[{"sku":"A-10","qty":2,"price":1520.30},{"sku":"B-7","qty":1,"price":89.90}],"coupon":null,"note":"deliver in the afternoon, \"blue\" gate"}"#;
        app.tabs[0].value_editor = Some(crate::app::ValueEditor {
            row: 2,
            col: 2,
            column: "data".into(),
            data_type: "jsonb".into(),
            text: crate::values::json_pretty(raw),
            original: Some(raw.into()),
            null: false,
            editable: true,
            json_column: true,
            compact: true,
        });
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/value-editor.png"))
            .unwrap();
        harness.state_mut().tabs[0].value_editor = None;
    }

    // Connected to a SQLite file: the sidebar shows the file instead of database/schema.
    {
        let s = harness.state_mut().session.as_mut().unwrap();
        s.cfg.kind = Kind::Sqlite;
        s.cfg.database = r"C:\Users\you\Documents\inventory.db".into();
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/sqlite-connected.png"))
            .unwrap();
        let s = harness.state_mut().session.as_mut().unwrap();
        s.cfg.kind = Kind::Postgres;
        s.cfg.database = String::new();
    }

    // Notices: an error stays (with copy and close), information fades.
    harness.state_mut().notice =
        crate::app::Notice::error("Couldn't do it: relation \"old_customers\" already exists");
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/toast-error.png"))
        .unwrap();
    harness.state_mut().notice = crate::app::Notice::info("Saved to C:\\Users\\you\\Downloads\\query.sql");
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/toast-info.png"))
        .unwrap();
    harness.state_mut().notice = None;

    // A view's tab has the eye, like the sidebar.
    {
        harness.state_mut().open_table("public", "monthly_sales");
        // It stays loading (no database here): a few frames, not "until it settles".
        harness.run_steps(3);
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/view-tab.png"))
            .unwrap();
        let app = harness.state_mut();
        app.tabs.pop();
        app.active = 0;
        // Its Structure tab: no designer, no CREATE TABLE "DDL"; the view's code instead.
        app.open_structure("public", "monthly_sales");
        if let Some(crate::app::Tab {
            kind: TabKind::Structure(st),
            ..
        }) = app.tabs.last_mut()
        {
            let col = |name: &str, ty: &str| crate::db::StructColumn {
                name: name.into(),
                data_type: ty.into(),
                nullable: true,
                default: None,
                pk: false,
            };
            st.data = Some(crate::db::TableStructure {
                columns: vec![
                    col("month", "date"),
                    col("orders", "bigint"),
                    col("total", "numeric"),
                ],
                ddl: "CREATE TABLE \"public\".\"monthly_sales\" (…);".into(),
                ..Default::default()
            });
        }
        harness.run_steps(3);
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/view-structure.png"))
            .unwrap();
        let app = harness.state_mut();
        app.tabs.pop();
        app.active = 0;
    }

    // Many tabs: the strip scrolls and keeps the active one in view.
    {
        let app = harness.state_mut();
        for i in 0..24 {
            app.open_sql(format!("SELECT {i};"));
        }
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/many-tabs.png"))
            .unwrap();
        let app = harness.state_mut();
        app.tabs.truncate(2);
        app.active = 0;
    }

    // "Compare structure" against another connection, with differences.
    {
        harness.state_mut().open_compare(None);
        let app = harness.state_mut();
        if let Some(Tab {
            kind: TabKind::Compare(st),
            ..
        }) = app.tabs.last_mut()
        {
            use crate::compare::{Item, Status, TableDiff};
            let it = |what: &str, sql: &str, destructive| Item {
                what: what.into(),
                sql: sql.into(),
                destructive,
            };
            st.target_conn = "staging".into();
            st.target_db = "shop".into();
            st.target_schema = "public".into();
            st.compared = "dev · postgres · public → staging · shop · public".into();
            st.same_connection = false;
            st.result = Some(Ok(vec![
                TableDiff {
                    table: "customers".into(),
                    status: Status::Changed,
                    items: vec![
                        it(
                            "column email: text → character varying(120)",
                            "ALTER TABLE …",
                            false,
                        ),
                        it("new column active", "ALTER TABLE …", false),
                        it(
                            "column fax only exists in the target (its data is lost)",
                            "ALTER TABLE …",
                            true,
                        ),
                        it("index customers_email_idx (new)", "CREATE INDEX …", false),
                    ],
                },
                TableDiff {
                    table: "coupons".into(),
                    status: Status::New,
                    items: vec![it("new table", "CREATE TABLE …", false)],
                },
                TableDiff {
                    table: "old_log".into(),
                    status: Status::Extra,
                    items: vec![it("only in the target", "-- DROP TABLE …", true)],
                },
            ]));
        }
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/compare.png"))
            .unwrap();
        let app = harness.state_mut();
        app.tabs.pop();
        app.active = 0;
    }

    // "Export .sql" of a schema, finished.
    {
        harness
            .state_mut()
            .open_dump(crate::app::DumpScope::Schema("public".into()));
        let app = harness.state_mut();
        if let Some(Tab {
            kind: TabKind::Dump(st),
            ..
        }) = app.tabs.last_mut()
        {
            st.progress = crate::dump::DumpProgress {
                tables: 3,
                total: 3,
                rows: 31_412,
                bytes: 4_812_000,
            };
            st.took = Some(std::time::Duration::from_millis(1_870));
            st.outcome = Some(Ok("C:\\Users\\you\\Downloads\\public_20261014-170501.sql".into()));
        }
        harness.run();
        harness.render().unwrap().save(format!("{dir}/dump.png")).unwrap();
        let app = harness.state_mut();
        app.tabs.pop();
        app.active = 0;
    }

    // "Filter values" on a table column, with a funnel on a column already filtered.
    {
        let state = harness.state_mut();
        state.active = 0;
        if let TabKind::Table(ts) = &mut state.tabs[0].kind {
            let v = |x: &str, n| (Some(x.to_string()), n);
            ts.value_conds.push(crate::app::ValueCond {
                column: "active".into(),
                cond: "\"active\" IN ('t')".into(),
                unchecked: Default::default(),
            });
            ts.value_filter = Some(crate::app::ValueFilter {
                column: "name".into(),
                expr: "\"name\"".into(),
                values: Some(Ok(vec![
                    v("Anna Smith", 412),
                    v("Bruce Jones", 388),
                    v("Carol Brooks", 301),
                    (None, 57),
                    v("Derek Clark", 12),
                ])),
                complete: true,
                unchecked: [None, Some("Derek Clark".to_string())].into_iter().collect(),
                search: String::new(),
                focused: true,
            });
        }
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/value-filter.png"))
            .unwrap();
        if let TabKind::Table(ts) = &mut harness.state_mut().tabs[0].kind {
            ts.value_filter = None;
            ts.value_conds.clear();
        }
    }

    // An error marked where it is in the editor.
    {
        let state = harness.state_mut();
        state.active = 1;
        let tab = &mut state.tabs[1];
        let saved = tab.result.take();
        let ed = tab.editor_mut().unwrap();
        let old = std::mem::replace(
            &mut ed.text,
            "SELECT id, name, emial\nFROM customers\nWHERE active;".into(),
        );
        ed.error_at = Some((17, 22));
        tab.error = Some("column \"emial\" does not exist (hint: Perhaps you meant to reference the column \"customers.email\".) (line 1, column 18)".into());
        harness.run();
        harness
            .render()
            .unwrap()
            .save(format!("{dir}/sql-error.png"))
            .unwrap();
        let tab = &mut harness.state_mut().tabs[1];
        tab.error = None;
        tab.result = saved;
        let ed = tab.editor_mut().unwrap();
        ed.text = old;
        ed.error_at = None;
    }

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
    harness.state_mut().dialog = Some(crate::app::Dialog::TableAction(crate::app::TableActionPrompt {
        schema: "public".into(),
        table: "customers".into(),
        action: crate::sql::TableAction::Drop,
        input: "clien".into(),
    }));
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/table-action.png"))
        .unwrap();
    harness.state_mut().dialog = None;
    {
        let file = std::env::temp_dir().join("migration_2026_10.sql");
        std::fs::write(&file, "x".repeat(48_000)).unwrap();
        harness.state_mut().open_script(&file);
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
                message: "db error: ERROR: column \"phone\" of relation \"customers\" already exists".into(),
                line: 418,
                statement: "ALTER TABLE customers ADD COLUMN phone text".into(),
                cancelled: false,
            }));
            let stmt = |line: usize, ms: u64, rows: u64, sql: &str| crate::db::StatementTime {
                line,
                sql: sql.into(),
                elapsed: std::time::Duration::from_millis(ms),
                rows,
                returned_rows: false,
            };
            st.slowest = vec![
                stmt(
                    37,
                    1_412,
                    0,
                    "CREATE INDEX orders_customer_idx ON orders (customer_id)",
                ),
                stmt(12, 388, 25_000, "COPY customers (id, name, email) FROM stdin"),
                stmt(
                    201,
                    96,
                    0,
                    "UPDATE orders SET status = 'archived' WHERE created_at < '2024-01-01'",
                ),
                stmt(
                    3,
                    41,
                    0,
                    "CREATE TABLE audit_log (id bigserial PRIMARY KEY, payload jsonb)",
                ),
            ];
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
    my.name = "shop-production".into();
    my.host = "db.example.com".into();
    my.database = "shop".into();
    harness.state_mut().saved.push(my);
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/welcome.png"))
        .unwrap();

    let mut remote = ConnConfig::new(Kind::MySql);
    remote.host = "db.example.com".into();
    remote.read_only = true;
    remote.color = Some(crate::db::Tag::Red);
    remote.timeout_secs = 30;
    remote.ssh = Some(crate::db::SshConfig {
        host: "bastion.example.com".into(),
        user: "deploy".into(),
        ..Default::default()
    });
    harness.state_mut().form = Some(crate::app::ConnForm {
        editing: None,
        cfg: remote,
    });
    harness.run();
    harness.render().unwrap().save(format!("{dir}/form.png")).unwrap();
    // Named, and tested: the server's answer under the buttons.
    {
        let app = harness.state_mut();
        let form = app.form.as_mut().unwrap();
        form.cfg.name = "shop-production".into();
        form.cfg.ssh = None;
        let (_tx, rx) = std::sync::mpsc::channel();
        app.conn_test = Some(crate::app::ConnTest {
            key: form.cfg.test_key(),
            rx,
            result: Some(Err(
                "ERROR 1045 (28000): Access denied for user 'root'@'10.0.3.7' (using password: YES)".into(),
            )),
        });
    }
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/form-test.png"))
        .unwrap();
    harness.state_mut().conn_test = None;
    {
        let form = harness.state_mut().form.as_mut().unwrap();
        form.cfg.kind = Kind::Sqlite;
        form.cfg.name = "inventory".into();
        form.cfg.database = r"C:\Users\you\Documents\inventory.db".into();
    }
    harness.run();
    harness
        .render()
        .unwrap()
        .save(format!("{dir}/form-sqlite.png"))
        .unwrap();
}
