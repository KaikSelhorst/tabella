use super::tests::fake_session;
use super::*;

fn app_with(session: Session) -> App {
    let dir = std::env::temp_dir().join("tabella-poll-tests-config");
    crate::store::use_dir_in_this_thread(dir);
    let mut app = App::new(super::window::NoWindow);
    app.session = Some(session);
    app
}

#[test]
fn a_dead_worker_ends_the_session_with_an_error() {
    let (s, _jobs, replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_sql(String::new());
    app.tabs[0].pending = 1;
    drop(replies);
    app.poll();
    assert!(app.session.is_none());
    assert!(app.notice.as_ref().is_some_and(|n| n.error));
    assert!(!app.tabs[0].busy(), "nothing stays stuck on Loading…");
}

/// Pinned tabs sit first in their strip, survive "Close others" and come back with the session.
#[test]
fn pinned_tabs_stay_first_and_are_kept() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_sql("select 1".into());
    app.open_sql("select 2".into());
    app.open_sql("select 3".into());
    let ids: Vec<u64> = app.tabs.iter().map(|t| t.id).collect();
    app.active = 2;
    app.set_pinned(2, true);
    assert_eq!(
        app.tabs.iter().map(|t| t.id).collect::<Vec<_>>(),
        [ids[2], ids[0], ids[1]]
    );
    assert_eq!(
        app.tabs[app.active].id, ids[2],
        "the active tab is still the same one"
    );
    // Dragging an unpinned tab before the pinned one puts it right after it.
    app.reorder_tab(ids[1], Some(ids[2]), None);
    assert_eq!(app.tabs[0].id, ids[2]);
    // "Close others" from an unpinned tab leaves the pinned one.
    let keep = app.tabs.iter().position(|t| t.id == ids[0]).unwrap();
    app.close_others(keep, false);
    assert_eq!(
        app.tabs.iter().map(|t| t.id).collect::<Vec<_>>(),
        [ids[2], ids[0]]
    );
    // The session remembers it.
    let snap = app.snapshot();
    assert_eq!(snap.pinned, [true, false]);
    let (s2, _j2, _r2) = fake_session(Kind::Postgres);
    let mut fresh = app_with(s2);
    fresh.restore_tabs(snap);
    assert_eq!(
        fresh.tabs.iter().map(|t| t.pinned).collect::<Vec<_>>(),
        [true, false]
    );
}

/// A generated UPDATE opens in its own editor with the preview already asked for.
#[test]
fn a_bulk_update_opens_previewed_in_an_editor() {
    let (s, jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    let sql = "UPDATE t SET a = a + 1 WHERE id IN ('1');".to_string();
    app.open_update_preview(sql.clone());
    let tab = app.tabs.last().unwrap();
    assert_eq!(tab.editor().unwrap().text, sql);
    assert_eq!(
        tab.editor().unwrap().preview.as_ref().map(|p| p.sql.clone()),
        Some(sql)
    );
    assert!(jobs.try_iter().any(|j| matches!(j, Job::Preview { .. })));
    assert!(tab.busy(), "waiting for the preview");
}

/// Starring is per connection and survives toggling the same table twice.
#[test]
fn tables_are_starred_per_connection() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.library.starred.clear();
    let id = app.session.as_ref().unwrap().cfg.id.clone();
    app.toggle_star("public", "orders");
    assert_eq!(
        app.library.starred[&id],
        [("public".to_string(), "orders".to_string())]
    );
    app.toggle_star("public", "orders");
    assert!(app.library.starred.is_empty());
}

/// Opening a table makes it the latest recent one, and the list is per connection.
#[test]
fn opened_tables_are_remembered_as_recent() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.library.recent.clear();
    app.open_table("public", "orders");
    app.open_table("public", "customers");
    app.open_table("public", "orders");
    assert_eq!(app.recent_rank("public", "orders"), Some(0));
    assert_eq!(app.recent_rank("public", "customers"), Some(1));
    assert_eq!(app.recent_rank("public", "products"), None);
}

/// Resuming a Postgres session goes back to the database it was in, once.
#[test]
fn a_resumed_postgres_session_returns_to_its_database() {
    let (mut s, _jobs, _replies) = fake_session(Kind::Postgres);
    s.cfg.database = "b".into();
    let mut app = app_with(s);
    let mut saved_cfg = ConnConfig::new(Kind::Postgres);
    saved_cfg.id = "x".into();
    saved_cfg.name = "x".into();
    saved_cfg.database = "a".into();
    app.connections.saved = vec![saved_cfg];
    let mut snap = app.snapshot();
    assert_eq!(snap.database.as_deref(), Some("b"));
    snap.conn = Some("x".into());
    app.session = None;
    app.restore = Some(snap);
    app.connect_saved_now("x");
    assert_eq!(app.session.as_ref().unwrap().cfg.database, "b");
    assert!(app.restore.as_ref().unwrap().database.is_none(), "used once");
    app.connect_saved_now("x");
    assert_eq!(
        app.session.as_ref().unwrap().cfg.database,
        "a",
        "then the saved one"
    );
}

/// `--conn` with a name that isn't saved says so instead of connecting somewhere else.
#[test]
fn an_unknown_connection_on_the_command_line_is_reported() {
    let dir = std::env::temp_dir().join("tabella-launch-tests-config");
    crate::store::use_dir_in_this_thread(dir);
    let launch = crate::cli::Launch {
        conn: Some("nowhere".into()),
        ..Default::default()
    };
    let app = App::with_launch(super::window::NoWindow, launch);
    assert!(app.session.is_none());
    let n = app.notice.as_ref().expect("a notice");
    assert!(n.error && n.text.contains("nowhere"), "{}", n.text);
    // A URL connects without being saved.
    let launch = crate::cli::Launch {
        url: Some("sqlite::memory:".into()),
        ..Default::default()
    };
    let app = App::with_launch(super::window::NoWindow, launch);
    assert_eq!(app.session.as_ref().map(|s| s.cfg.kind), Some(Kind::Sqlite));
    assert!(
        app.connections
            .saved
            .iter()
            .all(|c| c.kind != Kind::Sqlite || c.database != ":memory:")
    );
}

/// "Back" returns to the tab a foreign key was followed from and puts the old filter back.
#[test]
fn going_back_undoes_following_a_foreign_key() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_table("public", "orders");
    app.open_table("public", "customers");
    let filter_of = |app: &App, i: usize| match &app.tabs[i].kind {
        TabKind::Table(ts) => ts.filter.clone(),
        _ => unreachable!(),
    };
    if let TabKind::Table(ts) = &mut app.tabs[1].kind {
        ts.filter = "active".into();
    }
    app.active = 0;
    assert!(!app.can_go_back());
    // From orders to customers (already open): its filter changes.
    app.open_table_filtered("public", "customers", "id = 7".into());
    assert_eq!((app.active, filter_of(&app, 1).as_str()), (1, "id = 7"));
    // And on to a table that wasn't open.
    app.open_table_filtered("public", "regions", "id = 2".into());
    assert_eq!(app.tabs.len(), 3);
    app.go_back();
    assert_eq!(app.active, 1, "back to customers");
    app.go_back();
    assert_eq!(app.active, 0, "back to orders");
    assert_eq!(filter_of(&app, 1), "active", "the earlier filter returns");
    assert!(!app.can_go_back());
    // A closed origin is skipped.
    app.active = 0;
    app.open_table_filtered("public", "customers", "id = 1".into());
    app.tabs.remove(0);
    app.go_back();
    assert!(!app.can_go_back());
}

#[test]
fn session_snapshot_restores_the_same_tabs() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_sql("select 42".into());
    app.open_table("public", "customers");
    if let TabKind::Table(ts) = &mut app.tabs[1].kind {
        ts.filter = "active".into();
        ts.order = vec![("name".into(), false)];
    }
    app.open_structure("public", "orders");
    app.active = 1;
    let snap = app.snapshot();
    assert_eq!(snap.conn.as_deref(), Some("t"));

    let (s2, jobs2, _r2) = fake_session(Kind::Postgres);
    let mut fresh = app_with(s2);
    fresh.restore_tabs(snap);
    assert_eq!(fresh.snapshot().tabs, app.snapshot().tabs);
    assert_eq!(fresh.active, 1);
    // Restored tabs load their data like freshly opened ones.
    let kinds: Vec<_> = jobs2
        .try_iter()
        .map(|j| matches!(j, Job::Columns { .. } | Job::Structure { .. }))
        .collect();
    assert_eq!(kinds, [true, true]);
}

#[test]
fn a_tab_opens_with_the_widths_remembered_for_its_table() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.library.col_widths.clear();
    app.remember_widths("public", "customers", &[("name".into(), 250.0)]);
    app.open_table("public", "customers");
    app.open_table("public", "orders");
    assert_eq!(app.tabs[0].col_widths["name"], 250.0);
    assert!(
        app.tabs[1].col_widths.is_empty(),
        "another table starts with its own widths"
    );
    // They are on disk for the next run.
    let saved = crate::widths::load().value;
    assert_eq!(
        crate::widths::for_table(&saved, "t", "public", "customers")["name"],
        250.0
    );
    app.library.col_widths.clear();
    let _ = crate::widths::save(&mut app.library.col_widths);
}

#[test]
fn the_split_view_comes_back_with_the_session() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_table("public", "a");
    app.open_table("public", "b");
    app.open_table("public", "c");
    app.split_tab(2);
    app.active = 1;
    app.sync_split();
    let snap = app.snapshot();
    let sp = snap.split.as_ref().unwrap();
    assert_eq!(sp.right, [false, false, true]);
    assert_eq!(sp.shown, [1, 2]);
    assert_eq!(snap.active, 1);

    let (s2, _jobs2, _r2) = fake_session(Kind::Postgres);
    let mut fresh = app_with(s2);
    fresh.restore_tabs(snap);
    assert_eq!(
        fresh.tabs.iter().map(|t| t.right).collect::<Vec<_>>(),
        [false, false, true]
    );
    assert_eq!(fresh.active, 1);
    let shown = fresh.split.as_ref().unwrap().shown;
    assert_eq!(shown, [fresh.tabs[1].id, fresh.tabs[2].id]);
    // No split in the session: one pane, whatever the tabs were.
    let mut app = app_with(fake_session(Kind::Postgres).0);
    app.open_table("public", "a");
    assert!(app.snapshot().split.is_none());
}

#[test]
fn sql_files_open_save_and_guard_unsaved_changes() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    let dir = std::env::temp_dir().join(format!("tabella-tabfile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("query.sql");
    std::fs::write(&path, "SELECT 1;").unwrap();
    app.open_file(path.clone());
    assert_eq!(app.tabs[0].title, "query.sql");
    assert!(!file_dirty(&app.tabs[0]));
    if let Some(text) = app.tabs[0].editor_mut().map(|ed| &mut ed.text) {
        text.push_str("\nSELECT 2;");
    }
    assert!(file_dirty(&app.tabs[0]));
    app.close_tab(0);
    assert_eq!(app.tabs.len(), 1, "the first close only warns");
    app.save_file(0, false);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "SELECT 1;\nSELECT 2;");
    assert!(!file_dirty(&app.tabs[0]));
    // Opening it again focuses the tab instead of a second copy.
    app.open_sql(String::new());
    app.open_file(path.clone());
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active, 0);
    // The session remembers the file.
    let snap = app.snapshot();
    assert!(matches!(&snap.tabs[0], SavedTab::Sql { file: Some(f), .. } if *f == path));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn split_view_has_a_strip_per_pane_and_ends_when_one_empties() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_table("public", "a");
    app.open_table("public", "b");
    app.open_table("public", "c");
    let ids: Vec<u64> = app.tabs.iter().map(|t| t.id).collect();
    // Splitting a table tab moves it to the right pane, focused.
    app.split_tab(2);
    let sp = app.split.as_ref().unwrap();
    assert_eq!(sp.shown, [ids[1], ids[2]]);
    assert_eq!(app.active, 2);
    assert_eq!(app.pane_tabs(true).collect::<Vec<_>>(), [2]);
    assert_eq!(app.pane_tabs(false).collect::<Vec<_>>(), [0, 1]);
    // A tab opened now lands in the focused pane.
    app.open_table("public", "d");
    assert!(app.tabs[3].right);
    // Moving a tab across panes focuses it; the pane it left falls back to another tab.
    app.move_to_pane(1, true);
    assert_eq!(app.pane_tabs(false).collect::<Vec<_>>(), [0]);
    assert_eq!(app.split.as_ref().unwrap().shown[0], ids[0]);
    assert_eq!(app.active, 1);
    // Emptying a pane ends the split and keeps every tab.
    app.move_to_pane(0, true);
    assert!(app.split.is_none());
    assert_eq!(app.tabs.len(), 4);
    assert!(app.tabs.iter().all(|t| !t.right));
}

#[test]
fn splitting_a_sql_tab_copies_it() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_sql("select 1".into());
    app.split_tab(0);
    assert_eq!(app.tabs.len(), 2, "the original stays in the left pane");
    assert!(!app.tabs[0].right && app.tabs[1].right);
    assert_eq!(app.tabs[1].editor().unwrap().text, "select 1");
    // A lone non-SQL tab can't be split: it would leave the left pane empty.
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_table("public", "a");
    app.split_tab(0);
    assert!(app.split.is_none());
}

#[test]
fn closed_tabs_come_back_with_their_name() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_sql("select 1".into());
    app.tabs[0].title = "Report".into();
    app.tabs[0].renamed = true;
    app.open_table("public", "customers");
    if let TabKind::Table(ts) = &mut app.tabs[1].kind {
        ts.filter = "active".into();
    }
    app.close_tab(0);
    app.close_tab(0);
    assert!(app.tabs.is_empty());
    app.reopen_closed_tab();
    assert!(
        matches!(&app.tabs[0].kind, TabKind::Table(ts) if ts.filter == "active"),
        "last closed first"
    );
    app.reopen_closed_tab();
    assert_eq!(app.tabs[1].title, "Report");
    app.duplicate_tab(1);
    assert_eq!(app.tabs[2].title, "Report (copy)");
    app.close_others(2, false);
    assert_eq!(app.tabs.len(), 1);
}

/// A compact JSON opens laid out, is edited there and goes back compact as a pending
/// change; a read-only result opens it only to look.
#[test]
fn big_value_editor_round_trip() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = super::tests::table_tab();
    if let TabKind::Table(ts) = &mut tab.kind {
        ts.cols.push(ColumnInfo {
            enum_values: Vec::new(),
            name: "data".into(),
            data_type: "jsonb".into(),
            pk: false,
            nullable: true,
            has_default: false,
            references: None,
        });
    }
    let res = tab.result.as_mut().unwrap();
    res.rows.push([Some("8"), Some("t"), Some("beth")]);
    res.columns.push("data".into());
    res.rows = res
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut row = row.to_vec();
            row.push((i == 0).then(|| r#"{"price":1520.30,"tags":["a"]}"#.to_string()));
            row
        })
        .collect();
    let col = res.columns.len() - 1;
    handle_grid_event(&mut tab, &s, GridEvent::OpenValue(0, col), true);
    let ve = tab.value_editor.as_mut().unwrap();
    assert!(ve.editable && ve.json_column && ve.compact);
    assert_eq!(
        ve.text,
        "{\n  \"price\": 1520.30,\n  \"tags\": [\n    \"a\"\n  ]\n}"
    );
    assert_eq!(ve.value(), ve.original, "opening alone changes nothing");
    ve.text = ve.text.replace("1520.30", "99.90");
    let v = ve.value();
    assert_eq!(v.as_deref(), Some(r#"{"price":99.90,"tags":["a"]}"#));
    tab.value_editor = None;
    handle_grid_event(&mut tab, &s, GridEvent::Commit(0, col, v), true);
    assert!(has_unsaved(&tab));
    // Not editable: opens anyway, to read.
    handle_grid_event(&mut tab, &s, GridEvent::OpenValue(1, col), false);
    let ve = tab.value_editor.as_ref().unwrap();
    assert!(!ve.editable && ve.null);
}

/// Editing a foreign-key cell opens the picker (a small table loaded whole, a big one
/// searched); an answer for an older search is ignored.
#[test]
fn foreign_key_cells_open_the_picker() {
    let (mut s, jobs, replies) = fake_session(Kind::Postgres);
    let table = |name: &str, n: i64| crate::db::TableRef {
        schema: "public".into(),
        name: name.into(),
        is_view: false,
        estimate: Some(n),
        search_key: name.into(),
        size: None,
    };
    s.tables = vec![table("customers", 1_000_000), table("status", 5)];
    let col = |t: &str, c: &str, ty: &str| crate::db::SchemaColumn {
        schema: "public".into(),
        table: t.into(),
        column: c.into(),
        data_type: ty.into(),
        pk: c == "id",
    };
    s.schema_cols = vec![
        col("customers", "id", "integer"),
        col("customers", "name", "text"),
        col("status", "id", "integer"),
        col("status", "description", "text"),
    ];
    let mut app = app_with(s);
    app.open_table_filtered("public", "orders", String::new());
    let fk = |t: &str| crate::db::FkRef {
        schema: "public".into(),
        table: t.into(),
        column: "id".into(),
    };
    if let TabKind::Table(ts) = &mut app.tabs[0].kind {
        ts.cols = vec![
            ColumnInfo {
                enum_values: Vec::new(),
                name: "id".into(),
                data_type: "integer".into(),
                pk: true,
                nullable: false,
                has_default: true,
                references: None,
            },
            ColumnInfo {
                enum_values: Vec::new(),
                name: "customer_id".into(),
                data_type: "integer".into(),
                pk: false,
                nullable: false,
                has_default: false,
                references: Some(fk("customers")),
            },
            ColumnInfo {
                enum_values: Vec::new(),
                name: "status_id".into(),
                data_type: "integer".into(),
                pk: false,
                nullable: true,
                has_default: false,
                references: Some(fk("status")),
            },
        ];
    }
    app.tabs[0].result = Some(QueryResult {
        columns: vec!["id".into(), "customer_id".into(), "status_id".into()],
        rows: vec![vec![Some("1".into()), Some("42".into()), Some("2".into())]].into(),
        ..Default::default()
    });
    let tab = app.tabs[0].id;
    let fk_sql = |jobs: &Receiver<Job>| {
        jobs.try_iter().find_map(|j| match j {
            Job::FkOptions { sql, .. } => Some(sql),
            _ => None,
        })
    };
    let s = app.session.as_ref().unwrap();
    handle_grid_event(&mut app.tabs[0], s, GridEvent::StartEdit(0, 2), true);
    let p = app.tabs[0].fk_picker.as_ref().unwrap();
    assert!(p.local && p.label.as_deref() == Some("description") && p.current.as_deref() == Some("2"));
    assert!(
        fk_sql(&jobs)
            .unwrap()
            .contains("ORDER BY \"description\" LIMIT 10001")
    );
    handle_grid_event(&mut app.tabs[0], s, GridEvent::StartEdit(0, 1), true);
    let p = app.tabs[0].fk_picker.as_ref().unwrap();
    assert!(!p.local && p.numeric_key && p.label.as_deref() == Some("name"));
    assert!(fk_sql(&jobs).unwrap().ends_with("LIMIT 50"));
    assert!(
        app.tabs[0].value_editor.is_none()
            && matches!(&app.tabs[0].kind, TabKind::Table(ts) if ts.edit.is_none())
    );
    // Typed "mar", then "mary": the answer for "mar" comes first and is ignored.
    app.tabs[0].fk_picker.as_mut().unwrap().search = "mary".into();
    fk_search(&mut app.tabs[0], app.session.as_ref().unwrap());
    let row = |k: &str, l: &str| vec![Some(k.to_string()), Some(l.to_string())];
    for (search, rows) in [
        ("mar", vec![row("7", "Mark")]),
        ("mary", vec![row("42", "Mary Smith")]),
    ] {
        replies
            .send(Reply::FkOptions {
                tab,
                search: search.into(),
                res: Ok(QueryResult {
                    rows: rows.into(),
                    ..Default::default()
                }),
            })
            .unwrap();
        app.poll();
        let p = app.tabs[0].fk_picker.as_ref().unwrap();
        let first = p
            .options
            .as_ref()
            .and_then(|o| o.as_ref().ok())
            .and_then(|o| o.first().cloned());
        if search == "mar" {
            assert!(first.is_none(), "stale answer ignored");
        } else {
            assert_eq!(first, Some(("42".into(), Some("Mary Smith".into()))));
        }
    }
}

/// "Load more" is offered for a truncated plain read, never for something that writes.
#[test]
fn load_more_only_for_a_truncated_read() {
    let mut tab = Tab::new(1, "t".into(), TabKind::Sql(Box::new(Editor::new(String::new()))));
    let set = |tab: &mut Tab, sql: &str, truncated: bool| {
        tab.editor_mut().unwrap().set_last_query(sql.into());
        tab.result = Some(QueryResult {
            columns: vec!["a".into()],
            truncated,
            ..Default::default()
        });
    };
    set(&mut tab, "SELECT a FROM t", true);
    assert!(can_load_more(&tab));
    set(&mut tab, "SELECT a FROM t", false);
    assert!(!can_load_more(&tab), "nothing more to load");
    set(&mut tab, "INSERT INTO t VALUES (1) RETURNING a", true);
    assert!(!can_load_more(&tab), "running a write again would repeat it");
    set(&mut tab, "SELECT 1; SELECT 2", true);
    assert!(!can_load_more(&tab), "one statement only");
}

/// Pinning picks what identifies a row: the primary key, else "id", else the first column.
#[test]
fn pinning_guesses_the_row_key() {
    let res = |cols: &[&str]| QueryResult {
        columns: cols.iter().map(|c| c.to_string()).collect(),
        ..Default::default()
    };
    let mut ed = Editor::new("SELECT 1".into());
    ed.set_last_query("SELECT code, ID FROM t".into());
    ed.pin(&res(&["code", "ID"]), Some("code".into()));
    assert_eq!(ed.compare_key.as_deref(), Some("code"));
    assert_eq!(ed.pinned.as_ref().unwrap().sql, "SELECT code, ID FROM t");
    ed.pin(&res(&["name", "ID"]), None);
    assert_eq!(ed.compare_key.as_deref(), Some("ID"));
    ed.pin(&res(&["name", "state"]), Some("id".into()));
    assert_eq!(ed.compare_key.as_deref(), Some("name"));
    assert!(!ed.comparing);
}

/// The cached overlay follows every change to the edits and to the result.
#[test]
fn overlay_cache_never_shows_stale_edits() {
    let mut ts = TableState::new("public", "t", None);
    ts.cols = vec![ColumnInfo {
        enum_values: Vec::new(),
        name: "id".into(),
        data_type: "integer".into(),
        pk: true,
        nullable: false,
        has_default: false,
        references: None,
    }];
    let res = QueryResult {
        columns: vec!["id".into(), "name".into()],
        rows: vec![
            vec![Some("1".into()), Some("a".into())],
            vec![Some("2".into()), Some("b".into())],
        ]
        .into(),
        ..Default::default()
    };
    let set = |row: &str, v: &str| Change::Set {
        key: vec![Some(row.into())],
        col: "name".into(),
        value: Some(v.into()),
        before: None,
    };
    assert!(ts.overlay(&res).is_none());
    ts.changes.push(set("1", "x"));
    assert_eq!(ts.overlay(&res).unwrap().result.rows.cell(0, 1), Some("x"));
    // Same number of changes, different value.
    ts.changes[0] = set("1", "y");
    assert_eq!(ts.overlay(&res).unwrap().result.rows.cell(0, 1), Some("y"));
    ts.changes.push(Change::Delete {
        key: vec![Some("2".into())],
    });
    assert!(ts.overlay(&res).unwrap().deleted.contains(&1));
    // A new result (same shape) is overlaid again.
    let mut other = res;
    other.rows.set(0, 1, Some("z"));
    other.elapsed = std::time::Duration::from_millis(9);
    let o = ts.overlay(&other).unwrap();
    assert_eq!(o.result.rows.cell(0, 1), Some("y"));
    assert!(o.changed.contains(&(0, 1)));
}

#[test]
fn errors_are_found_in_the_editor_text() {
    let e = crate::error::DbError::MySql(mysql::Error::MySqlError(mysql::MySqlError {
        state: "42S22".into(),
        message: "Unknown column 'emial' in 'field list'".into(),
        code: 1054,
    }));
    // The statement ran with Ctrl+Enter is the second one; accents before it count as chars.
    let text = "-- report\nSELECT 1;\nSELECT name, emial FROM customers;";
    let spot = locate_error(text, "SELECT name, emial FROM customers", &e).unwrap();
    assert_eq!((spot.line, spot.column), (3, 14));
    let chars: String = text
        .chars()
        .skip(spot.start)
        .take(spot.end - spot.start)
        .collect();
    assert_eq!(chars, "emial");
    // With :parameters bound, the SQL sent isn't in the editor: no place to mark.
    assert_eq!(
        locate_error(text, "SELECT name, emial FROM customers WHERE id = 1", &e),
        None
    );
}

/// A session file can be old, hand-edited or from a run that crashed half-way: whatever its
/// indices and lists say, the tabs come back consistent (the active one and the split's panes
/// are open tabs).
#[test]
fn any_saved_session_restores_consistently() {
    use crate::session::{Saved, SavedSplit, SavedTab};
    let mut g = crate::testgen::Gen::new(18);
    for _ in 0..100 {
        let tabs: Vec<SavedTab> = (0..g.below(5))
            .map(|_| match g.below(3) {
                0 => SavedTab::Sql {
                    text: g.text(),
                    title: (g.below(2) == 0).then(|| g.text_of(2)),
                    file: None,
                },
                1 => SavedTab::Table {
                    schema: g.text_of(1),
                    table: g.text_of(2),
                    filter: g.text_of(3),
                    order: (g.below(2) == 0).then(|| (g.text_of(1), true)),
                    then_by: vec![(g.text_of(1), false)],
                    hidden: vec![g.text_of(1)],
                    pinned: vec![g.text_of(1)],
                },
                _ => SavedTab::Structure {
                    schema: g.text_of(1),
                    table: g.text_of(1),
                },
            })
            .collect();
        let odd = |g: &mut crate::testgen::Gen| [0, 1, 2, 7, usize::MAX][g.below(5)];
        let saved = Saved {
            conn: Some("t".into()),
            database: None,
            schema: None,
            active: odd(&mut g),
            window: None,
            pinned: (0..odd(&mut g).min(6)).map(|i| i % 2 == 0).collect(),
            split: (g.below(2) == 0).then(|| SavedSplit {
                right: (0..odd(&mut g).min(6)).map(|i| i % 2 == 1).collect(),
                shown: [odd(&mut g), odd(&mut g)],
            }),
            tabs,
        };
        let (s, _jobs, _replies) = fake_session(Kind::Postgres);
        let mut app = app_with(s);
        app.restore_tabs(saved);
        let open = |id: u64| app.tabs.iter().any(|t| t.id == id);
        assert!(app.tabs.is_empty() || app.active < app.tabs.len());
        if let Some(sp) = &app.split {
            assert!(sp.shown.iter().all(|&id| open(id)), "split panes show open tabs");
        }
        let snap = app.snapshot();
        assert!(snap.tabs.is_empty() || snap.active < snap.tabs.len());
    }
}

/// Whatever the server says about whatever ran, the mark lands inside the editor text (the
/// editor slices there: a position between bytes of a char would abort the app).
#[test]
fn error_marks_stay_inside_any_text() {
    use crate::error::DbError;
    crate::testgen::each_text(16, 2000, |text, g| {
        let chars: Vec<char> = text.chars().collect();
        let (a, b) = (g.below(chars.len() + 1), g.below(chars.len() + 1));
        let sent: String = chars[a.min(b)..a.max(b)].iter().collect();
        let near = g.text_of(3);
        let errors = [
            DbError::MySql(mysql::Error::MySqlError(mysql::MySqlError {
                state: "42000".into(),
                message: format!("You have an error near '{near}' at line {}", g.below(4)),
                code: 1064,
            })),
            DbError::MySql(mysql::Error::MySqlError(mysql::MySqlError {
                state: "42S22".into(),
                message: format!("Unknown column '{near}' in 'field list'"),
                code: 1054,
            })),
            DbError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(1),
                Some(format!(
                    "near \"{near}\": syntax error at offset {}",
                    g.below(sent.len() + 2)
                )),
            )),
            DbError::Other(g.text()),
        ];
        for e in &errors {
            if let Some(spot) = locate_error(text, &sent, e) {
                assert!(spot.start <= spot.end && spot.end <= chars.len(), "{spot:?}");
                assert!(spot.line >= 1 && spot.column >= 1);
            }
        }
    });
}

#[test]
fn a_tab_is_timed_from_its_first_job_to_its_last_reply() {
    let mut tab = Tab::new(1, "t".into(), TabKind::Sql(Box::new(Editor::new(String::new()))));
    tab.begin_job();
    tab.begin_job();
    assert_eq!(tab.end_job(), None, "still waiting for one");
    assert!(tab.end_job().is_some(), "the last reply reports how long it took");
    assert_eq!(tab.end_job(), None, "a stray reply measures nothing");
    assert!(!tab.busy());
}

#[test]
fn editor_results_of_one_table_become_editable() {
    let (mut s, jobs, replies) = fake_session(Kind::Postgres);
    s.tables = vec![TableRef {
        schema: "public".into(),
        name: "customers".into(),
        is_view: false,
        estimate: None,
        search_key: "customers".into(),
        size: None,
    }];
    let mut app = app_with(s);
    app.open_sql(String::new());
    let sql = "SELECT id, name, upper(name) AS shout FROM Customers WHERE active".to_string();
    run_sql(&mut app.tabs[0], app.session.as_ref().unwrap(), sql.clone());
    let tab = app.tabs[0].id;
    let rows = QueryResult {
        columns: vec!["id".into(), "name".into(), "shout".into()],
        rows: vec![vec![Some("7".into()), Some("anna".into()), Some("ANNA".into())]].into(),
        ..Default::default()
    };
    replies
        .send(Reply::Rows {
            tab,
            res: Box::new(Ok(rows)),
            notice: None,
        })
        .unwrap();
    app.poll();
    // The unquoted name resolves to the sidebar's table and its columns are asked for.
    let asked = jobs.try_iter().any(|j| matches!(j, Job::Columns { ref schema, ref table, .. } if schema == "public" && table == "customers"));
    assert!(asked);
    let col = |name: &str, pk| ColumnInfo {
        enum_values: Vec::new(),
        name: name.into(),
        data_type: "text".into(),
        pk,
        nullable: true,
        has_default: false,
        references: None,
    };
    replies
        .send(Reply::Columns {
            tab,
            res: Ok(vec![col("id", true), col("name", false)]),
            incoming: vec![],
        })
        .unwrap();
    app.poll();
    let s = app.session.as_ref().unwrap();
    let t = &mut app.tabs[0];
    handle_grid_event(t, s, GridEvent::Commit(0, 1, Some("beth".into())), true);
    // An expression isn't a column of the table.
    handle_grid_event(t, s, GridEvent::Commit(0, 2, Some("X".into())), true);
    assert!(t.error.as_deref().is_some_and(|e| e.contains("shout")));
    assert!(has_unsaved(t));
    // Running something else first would lose the edit: refused.
    run_sql(t, s, "SELECT 1".into());
    assert!(t.error.as_deref().is_some_and(|e| e.contains("pending")));
    save_changes(t, s);
    let Some(Job::Batch { stmts, refresh, .. }) = jobs.try_iter().last() else {
        panic!("expected a batch")
    };
    assert_eq!(
        stmts[0].render(Kind::Postgres),
        "UPDATE \"public\".\"customers\" SET \"name\" = 'beth' WHERE \"id\" = '7'"
    );
    assert_eq!(refresh, sql, "then the same SELECT runs again");
}

/// Check values, apply, reopen with the same checks (and without its own condition in the
/// list query), then remove.
#[test]
fn value_filter_round_trip() {
    let (s, jobs, replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_table_filtered("public", "customers", "active".into());
    let tab = app.tabs[0].id;
    let distinct = |jobs: &Receiver<Job>| {
        jobs.try_iter()
            .find_map(|j| match j {
                Job::Distinct { sql, .. } => Some(sql),
                _ => None,
            })
            .expect("a Distinct job")
    };
    let reply = |replies: &super::tests::FakeReplies| {
        let row = |v: Option<&str>, n: &str| vec![v.map(String::from), Some(n.to_string())];
        replies
            .send(Reply::Distinct {
                tab,
                res: Ok(QueryResult {
                    rows: vec![row(Some("NY"), "50"), row(Some("CA"), "30"), row(None, "2")].into(),
                    ..Default::default()
                }),
            })
            .unwrap();
    };
    let s = app.session.as_ref().unwrap();
    open_value_filter(&mut app.tabs[0], s, "state");
    assert!(distinct(&jobs).contains("WHERE active GROUP BY 1"));
    reply(&replies);
    app.poll();
    fn t(app: &mut App) -> &mut TableState {
        match &mut app.tabs[0].kind {
            TabKind::Table(ts) => ts,
            _ => unreachable!(),
        }
    }
    let vf = t(&mut app).value_filter.as_mut().unwrap();
    assert!(vf.complete && vf.values.as_ref().unwrap().as_ref().unwrap().len() == 3);
    vf.unchecked.insert(Some("CA".into()));
    let s = app.session.as_ref().unwrap();
    apply_value_filter(&mut app.tabs[0], s);
    assert_eq!(
        t(&mut app).filter,
        "(active) AND (\"state\" NOT IN ('CA') OR \"state\" IS NULL)"
    );
    assert!(t(&mut app).value_filter.is_none());

    let s = app.session.as_ref().unwrap();
    open_value_filter(&mut app.tabs[0], s, "state");
    assert!(
        distinct(&jobs).contains("WHERE active GROUP BY 1"),
        "its own condition is left out"
    );
    let vf = t(&mut app).value_filter.as_ref().unwrap();
    assert!(vf.unchecked.contains(&Some("CA".into())));
    reply(&replies);
    app.poll();
    t(&mut app).value_filter.as_mut().unwrap().unchecked.clear();
    let s = app.session.as_ref().unwrap();
    apply_value_filter(&mut app.tabs[0], s);
    assert_eq!(t(&mut app).filter, "active");
    assert!(t(&mut app).value_conds.is_empty());
}

/// Progress keeps the tab busy, the end frees it; a closed tab's reply is dropped; the
/// connection's own replies reach the session.
#[test]
fn replies_reach_their_tab_or_the_session() {
    let (s, _jobs, replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    let file = std::env::temp_dir().join("tabella-poll-script.sql");
    std::fs::write(&file, "SELECT 1;").unwrap();
    app.open_script(&file);
    let tab = app.tabs[0].id;
    app.tabs[0].begin_job();
    let progress = worker::ScriptProgress {
        statements: 3,
        ..Default::default()
    };
    replies.send(Reply::ScriptProgress { tab, progress }).unwrap();
    replies.send(Reply::Count { tab: 999, res: Ok(1) }).unwrap();
    app.poll();
    assert!(app.tabs[0].busy());
    assert!(matches!(&app.tabs[0].kind, TabKind::Script(st) if st.progress.statements == 3));
    replies
        .send(Reply::ScriptDone {
            tab,
            progress,
            error: None,
            slowest: Vec::new(),
        })
        .unwrap();
    replies
        .send(Reply::Location(Ok((
            vec!["postgres".into()],
            "postgres".into(),
            "public".into(),
        ))))
        .unwrap();
    app.poll();
    assert!(!app.tabs[0].busy());
    assert!(matches!(&app.tabs[0].kind, TabKind::Script(st) if st.outcome == Some(None)));
    assert_eq!(app.session.as_ref().unwrap().current_schema, "public");
}

#[test]
fn a_zero_row_edit_is_reported() {
    let (s, _jobs, replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_sql(String::new());
    let id = app.tabs[0].id;
    app.tabs[0].pending = 1;
    let notice = Some("No rows were changed".to_string());
    replies
        .send(Reply::Rows {
            tab: id,
            res: Box::new(Ok(QueryResult::default())),
            notice,
        })
        .unwrap();
    app.poll();
    assert!(
        app.tabs[0]
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("No rows"))
    );
    assert!(!app.tabs[0].busy() && app.session.is_some());
}

/// Ctrl+click on a column opens its table's structure pointing at it; on a structure tab that
/// is already open, another column moves the pointer (and asks for a new scroll).
#[test]
fn a_column_opens_the_structure_pointing_at_it() {
    let (s, jobs, _replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    let focus = |app: &App| match &app.tabs[app.active].kind {
        TabKind::Structure(st) => (st.focus.clone(), st.focus_seq),
        _ => (None, 0),
    };
    app.open_structure_at("public", "orders", Some("status"));
    assert_eq!(focus(&app), (Some("status".into()), 1));
    assert!(jobs.try_iter().any(|j| matches!(j, Job::Structure { .. })));
    app.open_structure_at("public", "orders", Some("total"));
    assert_eq!(app.tabs.len(), 1, "the open tab is reused");
    assert_eq!(focus(&app), (Some("total".into()), 2));
    assert!(jobs.try_iter().next().is_none(), "nothing is loaded again");
    // Opening it plainly keeps the column it points at.
    app.open_structure("public", "orders");
    assert_eq!(focus(&app), (Some("total".into()), 2));
}

/// In manual-commit mode, cells saved from a grid stay marked until the transaction ends;
/// with auto-commit nothing is marked (it is already committed).
#[test]
fn saved_cells_stay_marked_until_the_transaction_ends() {
    let (mut s, jobs, replies) = fake_session(Kind::Postgres);
    s.txn.manual = true;
    let mut app = app_with(s);
    app.tabs.push(super::tests::table_tab());
    let save = |app: &mut App| {
        let s = app.session.as_ref().unwrap();
        let t = &mut app.tabs[0];
        handle_grid_event(t, s, GridEvent::Commit(0, 2, Some("beth".into())), true);
        save_changes(t, s);
        assert!(jobs.try_iter().any(|j| matches!(j, Job::Batch { .. })));
        let res = t.result.clone().unwrap();
        replies
            .send(Reply::Saved {
                tab: t.id,
                res: Box::new(Ok(res)),
            })
            .unwrap();
    };
    save(&mut app);
    replies
        .send(Reply::Tx(TxState {
            manual: true,
            open: true,
            writes: 1,
            failed: false,
        }))
        .unwrap();
    app.poll();
    let marked = |app: &App| {
        let s = app.session.as_ref().unwrap();
        let t = &app.tabs[0];
        s.uncommitted_in(editable_ref(&t.kind).unwrap(), t.result.as_ref().unwrap())
    };
    assert_eq!(marked(&app), HashSet::from([(0, 2)]), "the `name` cell of row 7");
    // Committed (or rolled back): nothing is pending any more.
    replies
        .send(Reply::Tx(TxState {
            manual: true,
            ..Default::default()
        }))
        .unwrap();
    app.poll();
    assert!(marked(&app).is_empty());
    // Auto-commit: the save is already final.
    app.session.as_mut().unwrap().txn.manual = false;
    save(&mut app);
    app.poll();
    assert!(marked(&app).is_empty());
}

#[test]
fn a_late_reply_does_not_replace_a_newer_one() {
    let (s, _jobs, replies) = fake_session(Kind::Postgres);
    let mut app = app_with(s);
    app.open_sql(String::new());
    let id = app.tabs[0].id;
    app.tabs[0].pending = 2;
    let newer = QueryResult {
        columns: vec!["n".into()],
        rows: vec![vec![Some("2".into())]].into(),
        ..Default::default()
    };
    // Job 6 ran on the idle connection and answered before job 5, still on the busy one.
    replies.send_for(
        6,
        Reply::Rows {
            tab: id,
            res: Box::new(Ok(newer)),
            notice: None,
        },
    );
    replies.send_for(
        5,
        Reply::Rows {
            tab: id,
            res: Box::new(Err(crate::error::DbError::Other("old".into()))),
            notice: None,
        },
    );
    app.poll();
    let t = &app.tabs[0];
    assert_eq!(t.result.as_ref().and_then(|r| r.rows.cell(0, 0)), Some("2"));
    assert!(t.error.is_none(), "{:?}", t.error);
    assert!(!t.busy(), "the late reply still ends its job");
}
