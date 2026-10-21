//! Background work started from a tab: copy, export, insert, compare, dump, scripts, search.

use super::*;

/// What the rows on screen are called when exported: a file base name, the table name for
/// INSERT statements and the table's columns. An editor result from one table (a plain
/// `SELECT … FROM t`) keeps that table's name; any other is just "table".
fn result_target(tab: &Tab, k: Kind) -> (String, String, Vec<ColumnInfo>) {
    let from = |ts: &TableState| {
        (
            ts.table.clone(),
            k.qualified(&ts.schema, &ts.table),
            ts.cols.clone(),
        )
    };
    match &tab.kind {
        TabKind::Table(ts) => from(ts),
        TabKind::Sql(ed) => match ed.source.as_deref().filter(|s| !s.cols.is_empty()) {
            Some(ts) => from(ts),
            None => ("query".to_string(), "table".to_string(), vec![]),
        },
        _ => ("query".to_string(), "table".to_string(), vec![]),
    }
}

/// Largest text put on the clipboard: beyond it a file is the right place for the result.
const MAX_COPY_BYTES: usize = 20 * 1024 * 1024;

/// The result as CSV, JSON or INSERTs, ready for the clipboard. `None` (with the reason on the
/// tab) when there is nothing to copy or it is too big.
pub fn copy_result(tab: &mut Tab, s: &Session, format: Format) -> Option<String> {
    let k = s.cfg.kind;
    let (_, name, cols) = result_target(tab, k);
    let res = tab.result.as_ref()?;
    let rows = res.rows.len() as u64;
    let text = export::render(format, k, &name, res, &cols)?;
    if text.len() > MAX_COPY_BYTES {
        tab.error = Some(format!(
            "The result is too big for the clipboard ({} MB): use Export to a file",
            text.len() / (1024 * 1024)
        ));
        return None;
    }
    tab.exported = None;
    tab.copied = Some((format.label(), rows));
    Some(text)
}

/// Writes the tab's rows to a file in the Downloads folder. `whole_table` reads the entire table
/// (with the tab's filter) in chunks; otherwise only what is on screen.
pub fn start_export(tab: &mut Tab, s: &Session, format: Format, whole_table: bool) {
    let k = s.cfg.kind;
    let (base, table_name, cols, source) = match &tab.kind {
        TabKind::Table(ts) if whole_table => {
            if let Err(e) = sql::check_filter(&ts.filter) {
                tab.error = Some(e);
                return;
            }
            let source = ExportSource::Table {
                schema: ts.schema.clone(),
                table: ts.table.clone(),
                filter: ts.effective_filter(k),
                order: ts.order.clone(),
                pk: ts.pk_names(),
            };
            (
                ts.table.clone(),
                k.qualified(&ts.schema, &ts.table),
                ts.cols.clone(),
                source,
            )
        }
        _ => {
            let Some(res) = &tab.result else { return };
            let (base, name, cols) = result_target(tab, k);
            (base, name, cols, ExportSource::Rows(res.clone()))
        }
    };
    let path = export::target_path(&base, format);
    tab.exported = None;
    tab.copied = None;
    let spec = ExportSpec {
        format,
        path,
        table_name,
        cols,
        source,
    };
    let id = tab.id;
    tab.begin_job();
    s.send(Job::Export { tab: id, spec });
}

/// Opens the "new row" form: columns with a default start as "default", the rest empty.
pub fn open_insert_form(tab: &mut Tab) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let empty = QueryResult {
        columns: ts.cols.iter().map(|c| c.name.clone()).collect(),
        ..Default::default()
    };
    let kinds = values::column_kinds(&empty, &ts.cols);
    ts.insert = Some(
        ts.cols
            .iter()
            .zip(kinds)
            .map(|(c, kind)| InsertField {
                name: c.name.clone(),
                references: c.references.clone(),
                data_type: c.data_type.clone(),
                kind,
                enum_values: c.enum_values.clone(),
                nullable: c.nullable,
                mode: if c.has_default {
                    FieldMode::Default
                } else {
                    FieldMode::Value
                },
                text: String::new(),
            })
            .collect(),
    );
}

/// "Duplicate row": the insert form filled with a row's values. Keys the database generates
/// stay on "Default", so the copy gets a new one.
pub fn open_duplicate_form(tab: &mut Tab, row: usize) {
    open_insert_form(tab);
    let Some(res) = &tab.result else { return };
    let TabKind::Table(ts) = &mut tab.kind else { return };
    // Pending edits included: the copy is of what the grid shows.
    let overlay = ts.overlay(res);
    let shown = overlay.as_ref().map_or(res, |o| &o.result);
    let Some(values) = shown.rows.get(row) else { return };
    let generated: Vec<&str> = ts
        .cols
        .iter()
        .filter(|c| c.pk && c.has_default)
        .map(|c| c.name.as_str())
        .collect();
    let Some(fields) = &mut ts.insert else { return };
    for f in fields.iter_mut() {
        if generated.contains(&f.name.as_str()) {
            continue;
        }
        let Some(ci) = shown.columns.iter().position(|c| *c == f.name) else {
            continue;
        };
        match values.cell(ci) {
            Some(v) => {
                f.mode = FieldMode::Value;
                f.text = values::display_bool_or(f.kind, v);
            }
            None if f.nullable => f.mode = FieldMode::Null,
            None => {}
        }
    }
}

/// INSERTs the form's row. Booleans are normalized per engine like grid edits.
pub fn submit_insert(tab: &mut Tab, s: &Session) {
    let k = s.cfg.kind;
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let Some(fields) = ts.insert.take() else { return };
    let mut values = Vec::new();
    for f in &fields {
        let v = match f.mode {
            FieldMode::Default => continue,
            FieldMode::Null => None,
            FieldMode::Value if f.kind == ValueKind::Bool => match sql::parse_bool(&f.text) {
                Some(b) => Some(sql::bool_value(k, b).to_string()),
                None => {
                    tab.error = Some(format!("{}: \"{}\" is not a boolean", f.name, f.text));
                    ts.insert = Some(fields);
                    return;
                }
            },
            FieldMode::Value if !f.enum_values.is_empty() => {
                match enum_value(k, &f.name, &f.enum_values, &f.text) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tab.error = Some(e);
                        ts.insert = Some(fields);
                        return;
                    }
                }
            }
            FieldMode::Value => Some(f.text.clone()),
        };
        values.push((f.name.clone(), v));
    }
    let stmt = sql::insert_values(k, &ts.schema, &ts.table, &values);
    exec(tab, s, stmt, true);
}

pub fn refresh_activity(tab: &mut Tab, s: &Session) {
    let TabKind::Activity(st) = &mut tab.kind else {
        return;
    };
    st.last_refresh = Some(Instant::now());
    let (id, view) = (tab.id, st.view);
    if view == ActivityView::Server {
        tab.begin_job();
        s.send(Job::ServerInfo { tab: id });
        return;
    }
    tab.send_rows_job(s, Job::Activity { tab: id, view });
}

/// Starts "Compare structure" with what the tab has chosen.
pub fn start_compare(tab: &mut Tab, s: &Session, saved: &[ConnConfig]) {
    let TabKind::Compare(st) = &mut tab.kind else {
        return;
    };
    if st.running || st.target_schema.trim().is_empty() {
        return;
    }
    let Some(cfg) = saved
        .iter()
        .find(|c| c.id == st.target_conn)
        .or(Some(&s.cfg).filter(|c| c.id == st.target_conn))
    else {
        return;
    };
    let target_db = st.target_db.trim().to_string();
    // The same connection compares in place: MySQL sees every database, Postgres only its own.
    let same = cfg.id == s.cfg.id && (s.cfg.kind == Kind::MySql || target_db == s.current_db);
    let target = (!same).then(|| {
        Box::new(ConnConfig {
            database: if target_db.is_empty() {
                cfg.database.clone()
            } else {
                target_db.clone()
            },
            ..cfg.clone()
        })
    });
    let schema_note = |conn: &str, db: &str, schema: &str| match s.cfg.kind {
        Kind::Postgres => format!("{conn} · {db} · {schema}"),
        Kind::MySql => format!("{conn} · {schema}"),
        Kind::Sqlite => conn.to_string(),
    };
    st.compared = format!(
        "{} → {}",
        schema_note(&s.cfg.name, &s.current_db, &st.source_schema),
        schema_note(&cfg.name, &target_db, st.target_schema.trim())
    );
    st.same_connection = same;
    st.running = true;
    st.result = None;
    st.saved = None;
    let job = Job::Compare {
        tab: tab.id,
        source_schema: st.source_schema.clone(),
        target,
        target_schema: st.target_schema.trim().to_string(),
    };
    tab.begin_job();
    s.send(job);
}

/// The SQL of a comparison, with where it must run.
pub fn compare_script(st: &CompareState) -> String {
    let Some(Ok(diffs)) = &st.result else {
        return String::new();
    };
    format!(
        "-- Structure comparison: {}\n-- Run it on the TARGET. Dropping tables is left commented out.\n{}\n",
        st.compared,
        crate::compare::script(diffs)
    )
}

/// Starts "Export .sql": a new file in Downloads named after what is exported.
pub fn start_dump(tab: &mut Tab, s: &Session) {
    let TabKind::Dump(st) = &mut tab.kind else { return };
    if st.running || (!st.structure && !st.data) {
        return;
    }
    let base = st.what.split_once(' ').map_or(st.what.as_str(), |(_, w)| w);
    let spec = crate::dump::DumpSpec {
        path: export::target_path(base, Format::Sql),
        tables: st.tables.clone(),
        objects_of: st.objects_of.clone(),
        structure: st.structure,
        data: st.data,
        drop: st.drop,
    };
    st.cancel = Arc::default();
    st.running = true;
    st.outcome = None;
    st.progress = Default::default();
    st.started = Some(Instant::now());
    st.took = None;
    let job = Job::Dump {
        tab: tab.id,
        spec,
        cancel: st.cancel.clone(),
    };
    tab.begin_job();
    s.send(job);
}

pub fn cancel_dump(tab: &mut Tab, s: &Session) {
    if let TabKind::Dump(st) = &mut tab.kind
        && st.running
    {
        st.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        s.cancel(tab.id);
    }
}

/// Runs the tab's script file.
pub fn start_script(tab: &mut Tab, s: &Session) {
    let TabKind::Script(st) = &mut tab.kind else {
        return;
    };
    if st.running {
        return;
    }
    st.cancel = Arc::default();
    st.running = true;
    st.outcome = None;
    st.progress = Default::default();
    st.started = Some(Instant::now());
    st.took = None;
    let job = Job::RunScript {
        tab: tab.id,
        path: st.path.clone(),
        transaction: st.transaction,
        cancel: st.cancel.clone(),
    };
    tab.begin_job();
    s.send(job);
}

/// Stops the script: after the current statement, or right away by cancelling it.
pub fn cancel_script(tab: &mut Tab, s: &Session) {
    if let TabKind::Script(st) = &mut tab.kind
        && st.running
    {
        st.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        s.cancel(tab.id);
    }
}

/// Starts looking for the tab's term in every table.
pub fn start_search(tab: &mut Tab, s: &Session) {
    let TabKind::Search(st) = &mut tab.kind else {
        return;
    };
    let term = st.term.trim().to_string();
    if term.is_empty() || st.running {
        return;
    }
    let (targets, skipped) = crate::search::targets(&s.tables, &s.schema_cols, &term, st.include_big);
    st.cancel = Arc::default();
    st.running = !targets.is_empty();
    st.done = 0;
    st.total = targets.len();
    st.skipped = skipped;
    st.hits.clear();
    st.errors.clear();
    if targets.is_empty() {
        return;
    }
    let job = Job::SearchValue {
        tab: tab.id,
        term,
        exact: st.exact,
        targets,
        cancel: st.cancel.clone(),
    };
    tab.begin_job();
    s.send(job);
}

/// Stops the search: after the current table, or right away by cancelling its query.
pub fn cancel_search(tab: &mut Tab, s: &Session) {
    if let TabKind::Search(st) = &mut tab.kind
        && st.running
    {
        st.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        s.cancel(tab.id);
    }
}

/// Zeroes the "slow queries" counters on the server.
pub fn reset_query_stats(tab: &mut Tab, s: &Session) {
    let TabKind::Activity(st) = &mut tab.kind else {
        return;
    };
    st.last_refresh = Some(Instant::now());
    let id = tab.id;
    tab.send_rows_job(s, Job::ResetQueryStats { tab: id });
}

/// "Suggest index" for the selected slow query. `schema` is where its unqualified tables are.
pub fn suggest_index(tab: &mut Tab, s: &Session, sql: String, schema: String) {
    let TabKind::Activity(st) = &mut tab.kind else {
        return;
    };
    st.advice = None;
    let id = tab.id;
    tab.send_rows_job(s, Job::SuggestIndex { tab: id, sql, schema });
}

/// Cancels (or terminates) the session confirmed in the dialog.
pub fn kill_session(tab: &mut Tab, s: &Session) {
    let TabKind::Activity(st) = &mut tab.kind else {
        return;
    };
    let Some((id, terminate)) = st.confirm_kill.take() else {
        return;
    };
    st.kill_error = None;
    st.last_refresh = Some(Instant::now());
    let (tab_id, view) = (tab.id, st.view);
    tab.send_rows_job(
        s,
        Job::Kill {
            tab: tab_id,
            id,
            terminate,
            view,
        },
    );
}

/// The sidebar's table for a name from a query: exact first, then ignoring case (unquoted
/// names fold in Postgres). Without a schema, the name must be unambiguous.
pub(super) fn resolve_table(s: &Session, schema: Option<&str>, table: &str) -> Option<(String, String)> {
    let pick = |exact: bool| {
        let same = |a: &str, b: &str| if exact { a == b } else { a.eq_ignore_ascii_case(b) };
        let found: Vec<&TableRef> = s
            .tables
            .iter()
            .filter(|t| !t.is_view && same(&t.name, table) && schema.is_none_or(|sc| same(&t.schema, sc)))
            .collect();
        match found[..] {
            [one] => Some((one.schema.clone(), one.name.clone())),
            // MySQL: the connection's database wins among same-named tables.
            _ => found
                .iter()
                .find(|t| schema.is_none() && t.schema == s.current_db)
                .map(|t| (t.schema.clone(), t.name.clone())),
        }
    };
    pick(true).or_else(|| pick(false))
}
