use std::sync::mpsc::channel;

use super::*;

/// A ready session whose jobs land in the returned receiver instead of a database. Keep the
/// returned sender alive: when it drops, `poll` treats the worker as gone.
pub fn fake_session(kind: Kind) -> (Session, Receiver<Job>, Sender<Reply>) {
    let (tx, jobs) = channel();
    let (replies, rx) = channel();
    let mut cfg = ConnConfig::new(kind);
    cfg.id = "t".into();
    cfg.name = "t".into();
    let s = Session {
        cfg,
        tx,
        rx,
        canceller: None,
        ready: true,
        tables: vec![],
        columns: vec![],
        schema_cols: vec![],
        fks: vec![],
        objects: vec![],
        databases: vec![],
        current_db: String::new(),
        current_schema: String::new(),
        txn: TxState::default(),
    };
    (s, jobs, replies)
}

pub(super) fn table_tab() -> Tab {
    let col = |name: &str, ty: &str, pk| ColumnInfo {
        enum_values: Vec::new(),
        name: name.into(),
        data_type: ty.into(),
        pk,
        nullable: !pk,
        has_default: false,
        references: None,
    };
    let mut ts = TableState::new("public", "t", None);
    ts.cols = vec![
        col("id", "integer", true),
        col("active", "boolean", false),
        col("name", "text", false),
    ];
    let mut tab = Tab::new(1, "t".into(), TabKind::Table(Box::new(ts)));
    tab.result = Some(QueryResult {
        columns: vec!["id".into(), "active".into(), "name".into()],
        rows: vec![vec![Some("7".into()), Some("f".into()), Some("anna".into())]],
        ..Default::default()
    });
    tab
}

fn enum_tab() -> Tab {
    let mut tab = table_tab();
    if let TabKind::Table(ts) = &mut tab.kind {
        ts.cols[2].enum_values = ["pending", "shipped", "It's done"].map(String::from).to_vec();
    }
    tab
}

#[test]
fn enums_open_a_picker_and_only_take_their_values() {
    let (s, jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = enum_tab();
    handle_grid_event(&mut tab, &s, GridEvent::StartEdit(0, 2), true);
    assert!(matches!(&tab.kind, TabKind::Table(ts) if ts.option_pick == Some((0, 2)) && ts.edit.is_none()));
    assert!(jobs.try_recv().is_err(), "opening the picker writes nothing");
    handle_grid_event(
        &mut tab,
        &s,
        GridEvent::Commit(0, 2, Some("It's done".into())),
        true,
    );
    assert!(tab.error.is_none());
    save_changes(&mut tab, &s);
    let Ok(Job::Batch { stmts, .. }) = jobs.try_recv() else {
        panic!()
    };
    assert_eq!(
        stmts[0].render(Kind::Postgres),
        "UPDATE \"public\".\"t\" SET \"name\" = 'It''s done' WHERE \"id\" = '7'"
    );
    // Anything else is refused before it reaches the server, with the values to choose from.
    let mut tab = enum_tab();
    handle_grid_event(
        &mut tab,
        &s,
        GridEvent::Commit(0, 2, Some("Shipped".into())),
        true,
    );
    let error = tab.error.clone().unwrap();
    assert!(
        error.contains("\"Shipped\" is not a value of name: pending, shipped"),
        "{error}"
    );
    let TabKind::Table(ts) = &tab.kind else { panic!() };
    assert!(ts.changes.is_empty());
    // NULL is always allowed for a nullable column.
    handle_grid_event(&mut tab, &s, GridEvent::Commit(0, 2, None), true);
    let TabKind::Table(ts) = &tab.kind else { panic!() };
    assert_eq!(ts.changes.len(), 1);
}

#[test]
fn mysql_enums_compare_ignoring_case_and_take_the_declared_spelling() {
    let (s, jobs, _replies) = fake_session(Kind::MySql);
    let mut tab = enum_tab();
    handle_grid_event(
        &mut tab,
        &s,
        GridEvent::Commit(0, 2, Some("SHIPPED".into())),
        true,
    );
    assert!(tab.error.is_none());
    save_changes(&mut tab, &s);
    let Ok(Job::Batch { stmts, .. }) = jobs.try_recv() else {
        panic!()
    };
    assert_eq!(stmts[0].with_placeholders().1[0], "shipped");
}

#[test]
fn the_insert_form_checks_enum_values_too() {
    let (s, jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = enum_tab();
    open_insert_form(&mut tab);
    let TabKind::Table(ts) = &mut tab.kind else {
        panic!()
    };
    let fields = ts.insert.as_mut().unwrap();
    assert_eq!(fields[2].enum_values.len(), 3);
    fields[0].mode = FieldMode::Default;
    fields[1].mode = FieldMode::Default;
    fields[2].mode = FieldMode::Value;
    fields[2].text = "nope".into();
    submit_insert(&mut tab, &s);
    assert!(tab.error.is_some());
    assert!(jobs.try_recv().is_err());
    let TabKind::Table(ts) = &mut tab.kind else {
        panic!()
    };
    assert!(ts.insert.is_some(), "the form stays open to fix the value");
    ts.insert.as_mut().unwrap()[2].text = "pending".into();
    submit_insert(&mut tab, &s);
    assert!(jobs.try_recv().is_ok());
}

#[test]
fn commit_writes_one_row_and_expects_it() {
    let (s, jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = table_tab();
    handle_grid_event(&mut tab, &s, GridEvent::Commit(0, 2, Some("beth".into())), true);
    assert!(jobs.try_recv().is_err(), "edits wait for Save");
    handle_grid_event(&mut tab, &s, GridEvent::DeleteRow(0), true);
    save_changes(&mut tab, &s);
    let Ok(Job::Batch { stmts, .. }) = jobs.try_recv() else {
        panic!("expected a batch")
    };
    let sql: Vec<String> = stmts.iter().map(|st| st.render(Kind::Postgres)).collect();
    assert_eq!(
        sql,
        [
            "UPDATE \"public\".\"t\" SET \"name\" = 'beth' WHERE \"id\" = '7'",
            "DELETE FROM \"public\".\"t\" WHERE \"id\" = '7'"
        ]
    );
    assert!(tab.busy());
}

#[test]
fn booleans_open_a_picker_and_are_normalized() {
    let (s, jobs, _replies) = fake_session(Kind::MySql);
    let mut tab = table_tab();
    handle_grid_event(&mut tab, &s, GridEvent::StartEdit(0, 1), true);
    assert!(matches!(&tab.kind, TabKind::Table(ts) if ts.option_pick == Some((0, 1))));
    assert!(jobs.try_recv().is_err(), "opening the picker writes nothing");
    handle_grid_event(&mut tab, &s, GridEvent::Commit(0, 1, Some("true".into())), true);
    save_changes(&mut tab, &s);
    let Ok(Job::Batch { stmts, .. }) = jobs.try_recv() else {
        panic!()
    };
    assert_eq!(
        stmts[0].with_placeholders().1[0],
        "1",
        "MySQL booleans are written as 1/0"
    );
    handle_grid_event(&mut tab, &s, GridEvent::Commit(0, 1, Some("maybe".into())), true);
    assert!(tab.error.is_some());
}

#[test]
fn paste_fills_cells_from_the_selection_and_copy_takes_the_range() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = table_tab();
    if let Some(r) = &mut tab.result {
        r.rows
            .push(vec![Some("8".into()), Some("t".into()), Some("beth".into())]);
    }
    handle_grid_event(&mut tab, &s, GridEvent::Select(0, 1), true);
    // Two rows of (active, name) from a spreadsheet; the third column overflows the page.
    let text = "yes\tcarol\textra\r\nNULL\tdan\r\n".to_string();
    handle_grid_event(&mut tab, &s, GridEvent::Paste(text), true);
    let TabKind::Table(ts) = &tab.kind else { panic!() };
    let sets: Vec<(String, Option<String>)> = ts
        .changes
        .iter()
        .map(|c| match c {
            Change::Set { col, value, .. } => (col.clone(), value.clone()),
            Change::Delete { .. } => panic!(),
        })
        .collect();
    assert_eq!(
        sets,
        [
            ("active".into(), Some("true".into())),
            ("name".into(), Some("carol".into())),
            ("active".into(), None),
            ("name".into(), Some("dan".into())),
        ]
    );
    assert!(tab.error.as_deref().is_some_and(|e| e.contains("1 pasted cell")));
    assert_eq!(
        tab.range(),
        Some(CellRange {
            r0: 0,
            r1: 1,
            cols: vec![1, 2]
        }),
        "the pasted block ends up selected"
    );
    // Copy shows what the grid shows: the pasted (unsaved) values.
    let Some(Effect::Copy(tsv)) =
        handle_grid_event(&mut tab, &s, GridEvent::CopyCells { headers: true }, true)
    else {
        panic!()
    };
    assert_eq!(tsv, "active\tname\ntrue\tcarol\n\tdan\n");
    handle_grid_event(&mut tab, &s, GridEvent::SelectAll, true);
    assert_eq!(
        tab.range(),
        Some(CellRange {
            r0: 0,
            r1: 1,
            cols: vec![0, 1, 2]
        })
    );
    // Hidden and pinned columns: ranges follow what's on screen.
    tab.hidden_cols = vec!["active".into()];
    tab.pinned_cols = vec!["name".into()];
    assert_eq!(tab.view(), [2, 0]);
    handle_grid_event(&mut tab, &s, GridEvent::SelectAll, true);
    assert_eq!(
        tab.range(),
        Some(CellRange {
            r0: 0,
            r1: 1,
            cols: vec![2, 0]
        })
    );
    handle_grid_event(&mut tab, &s, GridEvent::Select(1, 1), true);
    assert_eq!(tab.range(), None);
}

#[test]
fn editing_the_open_connection_applies_what_it_can() {
    let (mut s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut edited = s.cfg.clone();
    edited.preview_writes = true;
    edited.name = "production".into();
    assert!(apply_live_settings(&mut s, &edited), "nothing needs a reconnect");
    assert!(s.cfg.preview_writes && s.cfg.name == "production");
    edited.host = "other-server".into();
    assert!(
        !apply_live_settings(&mut s, &edited),
        "a new host waits for a reconnect"
    );
    assert_eq!(s.cfg.host, "localhost", "and isn't applied live");
}

#[test]
fn bulk_edits_on_a_selection() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = table_tab();
    if let Some(r) = &mut tab.result {
        r.rows
            .push(vec![Some("8".into()), Some("t".into()), Some("beth".into())]);
        r.rows
            .push(vec![Some("9".into()), Some("f".into()), Some("cole".into())]);
    }
    let sets = |tab: &Tab| -> Vec<(Vec<Option<String>>, String, Option<String>)> {
        let TabKind::Table(ts) = &tab.kind else { panic!() };
        ts.changes
            .iter()
            .filter_map(|c| match c {
                Change::Set { key, col, value, .. } => Some((key.clone(), col.clone(), value.clone())),
                Change::Delete { .. } => None,
            })
            .collect()
    };
    let key = |id: &str| vec![Some(id.to_string())];
    // "name" of the first two rows set to one value.
    handle_grid_event(&mut tab, &s, GridEvent::Select(0, 2), true);
    handle_grid_event(&mut tab, &s, GridEvent::Extend(1, 2), true);
    handle_grid_event(&mut tab, &s, GridEvent::FillSelection(Some("same".into())), true);
    assert_eq!(
        sets(&tab),
        [
            (key("7"), "name".into(), Some("same".into())),
            (key("8"), "name".into(), Some("same".into()))
        ]
    );
    // Fill down takes the first row as shown (with its pending edit).
    handle_grid_event(&mut tab, &s, GridEvent::Select(0, 2), true);
    handle_grid_event(&mut tab, &s, GridEvent::Extend(2, 2), true);
    handle_grid_event(&mut tab, &s, GridEvent::FillDown, true);
    assert!(sets(&tab).contains(&(key("9"), "name".into(), Some("same".into()))));
    // Delete marks every selected row once.
    handle_grid_event(&mut tab, &s, GridEvent::DeleteRows, true);
    handle_grid_event(&mut tab, &s, GridEvent::DeleteRows, true);
    let TabKind::Table(ts) = &tab.kind else { panic!() };
    assert_eq!(
        ts.changes
            .iter()
            .filter(|c| matches!(c, Change::Delete { .. }))
            .count(),
        3
    );
}

#[test]
fn read_only_tabs_ignore_edits_but_still_copy() {
    let (s, jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = table_tab();
    handle_grid_event(&mut tab, &s, GridEvent::Commit(0, 2, Some("x".into())), false);
    assert!(jobs.try_recv().is_err());
    let Some(Effect::Copy(json)) = handle_grid_event(&mut tab, &s, GridEvent::Copy(0, CopyAs::Json), false)
    else {
        panic!("expected a copy")
    };
    assert!(json.contains("\"id\": 7"));
}

#[test]
fn unsafe_filters_never_reach_the_database() {
    let (s, jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = table_tab();
    if let TabKind::Table(ts) = &mut tab.kind {
        ts.filter = "1=1; DELETE FROM t; SELECT 1 AS id".into();
    }
    reload(&mut tab, &s);
    handle_grid_event(&mut tab, &s, GridEvent::DeleteRow(0), true);
    assert!(jobs.try_recv().is_err());
    assert!(tab.error.is_some() && !tab.busy());
}

#[test]
fn insert_form_sends_only_what_was_filled() {
    let (s, jobs, _replies) = fake_session(Kind::MySql);
    let mut tab = table_tab();
    open_insert_form(&mut tab);
    let TabKind::Table(ts) = &mut tab.kind else {
        unreachable!()
    };
    let fields = ts.insert.as_mut().unwrap();
    // id has no default in the fixture: starts as a value to fill in.
    assert_eq!(fields[0].mode, FieldMode::Value);
    fields[0].text = "8".into();
    fields[1].mode = FieldMode::Value;
    fields[1].text = "yes".into();
    fields[2].mode = FieldMode::Default;
    submit_insert(&mut tab, &s);
    let _count = jobs.try_recv().unwrap();
    let Job::Exec { stmt, .. } = jobs.try_recv().unwrap() else {
        panic!()
    };
    let (sql, values) = stmt.with_placeholders();
    assert_eq!(sql, "INSERT INTO `public`.`t` (`id`, `active`) VALUES (?, ?)");
    assert_eq!(values, ["8", "1"]);
}

#[test]
fn a_bad_boolean_keeps_the_form_open() {
    let (s, jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = table_tab();
    open_insert_form(&mut tab);
    if let TabKind::Table(ts) = &mut tab.kind {
        let f = &mut ts.insert.as_mut().unwrap()[1];
        f.mode = FieldMode::Value;
        f.text = "maybe".into();
    }
    submit_insert(&mut tab, &s);
    assert!(jobs.try_recv().is_err());
    assert!(tab.error.is_some());
    assert!(matches!(&tab.kind, TabKind::Table(ts) if ts.insert.is_some()));
}

#[test]
fn exports_go_to_the_worker() {
    let (s, jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = table_tab();
    start_export(&mut tab, &s, Format::Csv, true);
    let Ok(Job::Export { spec, .. }) = jobs.try_recv() else {
        panic!("expected an export job")
    };
    assert!(matches!(spec.source, ExportSource::Table { .. }));
    assert_eq!(spec.path.extension().unwrap(), "csv");
    assert!(tab.busy());
    start_export(&mut tab, &s, Format::Json, false);
    let Ok(Job::Export { spec, .. }) = jobs.try_recv() else {
        panic!()
    };
    assert!(matches!(spec.source, ExportSource::Rows(ref r) if r.rows.len() == 1));
}

/// "Copy to clipboard" gives the result as CSV, JSON or INSERTs named after its table, and
/// leaves a note on the tab.
#[test]
fn the_result_can_be_copied_in_three_formats() {
    let (s, _jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = table_tab();
    let csv = copy_result(&mut tab, &s, Format::Csv).unwrap();
    assert_eq!(csv, "id,active,name\n7,f,anna\n");
    assert_eq!(tab.copied, Some(("CSV", 1)));
    let json = copy_result(&mut tab, &s, Format::Json).unwrap();
    assert!(json.contains("\"name\":\"anna\""), "{json}");
    let sql = copy_result(&mut tab, &s, Format::Sql).unwrap();
    assert!(sql.starts_with("INSERT INTO \"public\".\"t\""), "{sql}");
    // Excel is a file, not text.
    assert!(copy_result(&mut tab, &s, Format::Xlsx).is_none());
    // An editor result from nowhere in particular is just "table".
    let mut sql_tab = Tab::new(2, "q".into(), TabKind::Sql(Box::new(Editor::new(String::new()))));
    sql_tab.result = tab.result.clone();
    assert!(
        copy_result(&mut sql_tab, &s, Format::Sql)
            .unwrap()
            .starts_with("INSERT INTO table")
    );
}

#[test]
fn sort_cycles_and_reloads() {
    let (s, jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = table_tab();
    for expected in [Some(true), Some(false), None] {
        handle_grid_event(&mut tab, &s, GridEvent::Sort("name".into()), true);
        let TabKind::Table(ts) = &tab.kind else {
            unreachable!()
        };
        assert_eq!(ts.order.first().map(|(_, asc)| *asc), expected);
    }
    assert!(jobs.try_iter().any(|j| matches!(j, Job::Query { .. })));
}

/// Shift+click adds sort columns in the order they were clicked; each one flips, then leaves.
#[test]
fn shift_click_sorts_by_several_columns() {
    let (s, jobs, _replies) = fake_session(Kind::Postgres);
    let mut tab = table_tab();
    let order = |tab: &Tab| match &tab.kind {
        TabKind::Table(ts) => ts.order.clone(),
        _ => unreachable!(),
    };
    let name = |c: &str| (c.to_string(), true);
    handle_grid_event(&mut tab, &s, GridEvent::Sort("name".into()), true);
    handle_grid_event(&mut tab, &s, GridEvent::SortAlso("email".into()), true);
    assert_eq!(order(&tab), [name("name"), name("email")]);
    handle_grid_event(&mut tab, &s, GridEvent::SortAlso("email".into()), true);
    assert_eq!(order(&tab), [name("name"), ("email".to_string(), false)]);
    handle_grid_event(&mut tab, &s, GridEvent::SortAlso("name".into()), true);
    assert_eq!(
        order(&tab),
        [("name".to_string(), false), ("email".to_string(), false)]
    );
    handle_grid_event(&mut tab, &s, GridEvent::SortAlso("email".into()), true);
    assert_eq!(
        order(&tab),
        [("name".to_string(), false)],
        "a third click drops it"
    );
    // A plain click goes back to a single column.
    handle_grid_event(&mut tab, &s, GridEvent::SortAlso("email".into()), true);
    handle_grid_event(&mut tab, &s, GridEvent::Sort("id".into()), true);
    assert_eq!(order(&tab), [name("id")]);
    assert!(jobs.try_iter().any(|j| matches!(j, Job::Query { .. })));
}
