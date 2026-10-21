//! Replies from the connection's worker thread, applied to the session and the tabs.

use super::*;
use std::fmt::Write as _;

use crate::error::DbResult;

/// What a tab's reply may need besides the tab and the session.
struct TabReplyCx<'a> {
    notice: &'a mut Option<Notice>,
    history: &'a mut Vec<Entry>,
    /// Design tabs whose DDL ran: (tab, schema, table).
    designed: &'a mut Vec<(u64, String, String)>,
}

impl App {
    /// Applies every reply that arrived since the last frame.
    pub fn poll(&mut self) {
        if let Some(e) = crate::history::take_write_error() {
            self.notice = Notice::error(format!("Error saving history: {e}"));
        }
        let mut restore = None;
        let mut designed: Vec<(u64, String, String)> = Vec::new();
        // Tabs that just stopped being busy, and for how long they were.
        let mut finished: Vec<(String, std::time::Duration)> = Vec::new();
        loop {
            let Some(s) = &mut self.session else { return };
            let reply = match s.rx.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The worker thread ended unexpectedly (or failed to connect, reported above).
                    if s.ready {
                        self.notice = Notice::error("The connection was closed unexpectedly.");
                    }
                    self.session = None;
                    for t in &mut self.tabs {
                        t.pending = 0;
                    }
                    return;
                }
            };
            let Some((id, ends_job)) = reply.tab() else {
                if !self.on_session_reply(reply, &mut restore) {
                    return;
                }
                continue;
            };
            // The tab may have been closed meanwhile.
            let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else {
                continue;
            };
            if ends_job && let Some(took) = t.end_job() {
                finished.push((t.title.clone(), took));
            }
            let mut cx = TabReplyCx {
                notice: &mut self.notice,
                history: &mut self.history,
                designed: &mut designed,
            };
            on_tab_reply(t, s, reply, &mut cx);
        }
        if let Some(r) = restore {
            self.restore_tabs(r);
        }
        if self.session.as_ref().is_some_and(|s| s.ready) && !self.launch_files.is_empty() {
            for path in std::mem::take(&mut self.launch_files) {
                self.open_file(path);
            }
        }
        for (tab, schema, table) in designed {
            self.after_design(tab, &schema, &table);
        }
        self.notify_finished(&finished);
    }

    /// A reply about the connection itself. `false` when the session is gone.
    fn on_session_reply(&mut self, reply: Reply, restore: &mut Option<session::Saved>) -> bool {
        let Some(s) = &mut self.session else { return false };
        match reply {
            Reply::Tx(state) => s.txn = state,
            Reply::TxEnded { commit, res } => {
                if std::mem::take(&mut self.quit_after_tx) && res.is_ok() {
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                self.notice = match res {
                    Ok(()) if commit => Notice::info("Transaction committed"),
                    Ok(()) => Notice::info("Transaction rolled back"),
                    Err(e) => Notice::error(format!("Transaction: {e}")),
                };
                // What's on screen may include undone changes: reload table pages.
                for t in &mut self.tabs {
                    if matches!(t.kind, TabKind::Table(_)) && !t.busy() && !has_unsaved(t) {
                        reload(t, s);
                    }
                }
            }
            Reply::Connected(Ok(c)) => {
                if self
                    .restore
                    .as_ref()
                    .is_some_and(|r| r.conn.as_deref() == Some(s.cfg.id.as_str()))
                {
                    *restore = self.restore.take();
                }
                s.ready = true;
                s.tables = c.tables;
                s.canceller = Some(c.canceller);
                self.notice = Notice::info(format!("Connected to {}", s.cfg.name));
            }
            Reply::Reconnected {
                res: Ok(c),
                rolled_back,
            } => {
                s.tables = c.tables;
                s.canceller = Some(c.canceller);
                if rolled_back {
                    s.txn = TxState {
                        manual: s.txn.manual,
                        ..Default::default()
                    };
                }
                let tx = if rolled_back {
                    " The open transaction was lost: the server rolled it back."
                } else {
                    ""
                };
                self.notice =
                    Notice::info(format!("Connection to {} was lost and restored.{tx}", s.cfg.name));
            }
            Reply::Reconnected { res: Err(e), .. } => {
                self.notice = Notice::error(format!("Connection to {} was lost: {e}", s.cfg.name));
            }
            Reply::Connected(Err(e)) => {
                self.notice = Notice::error(format!("Failed to connect: {e}"));
                self.session = None;
                return false;
            }
            Reply::Tables(Ok(t)) => s.tables = t,
            Reply::Tables(Err(e)) => self.notice = Notice::error(e.to_string()),
            Reply::AllColumns(Ok(c)) => {
                s.columns = c.iter().map(|c| (c.table.clone(), c.column.clone())).collect();
                s.schema_cols = c;
            }
            // Autocomplete just has no columns then.
            Reply::AllColumns(Err(_)) => {}
            Reply::AllFks(Ok(f)) => s.fks = f,
            Reply::Objects(Ok(o)) => s.objects = o,
            Reply::Admin { done, res } => match res {
                Ok(()) => self.notice = Notice::info(done),
                Err(e) => self.notice = Notice::error(format!("Couldn't do it: {e}")),
            },
            Reply::Location(Ok((dbs, db, schema))) => {
                // A switch (not the first report at connect) focuses the new one.
                let (before, now) = match s.cfg.kind {
                    Kind::Postgres => (&s.current_schema, &schema),
                    Kind::MySql | Kind::Sqlite => (&s.current_db, &db),
                };
                if !before.is_empty() && before != now {
                    self.schema_focus = Some(now.clone());
                }
                s.databases = dbs;
                s.current_db = db;
                s.current_schema = schema;
            }
            Reply::Location(Err(e)) => self.notice = Notice::error(format!("Couldn't switch: {e}")),
            // The sidebar just shows tables then.
            Reply::Objects(Err(_)) => {}
            // Without them the editor just doesn't suggest joins.
            Reply::AllFks(Err(_)) => {}
            // `Reply::tab` sends these to `on_tab_reply`.
            _ => {}
        }
        true
    }
}

/// A reply to one tab (its job already counted as ended when it was the last).
fn on_tab_reply(t: &mut Tab, s: &Session, reply: Reply, cx: &mut TabReplyCx) {
    match reply {
        Reply::SearchProgress { hit, error, .. } => {
            if let TabKind::Search(st) = &mut t.kind {
                st.done += 1;
                st.hits.extend(hit);
                // A cancelled statement isn't an error worth listing.
                if !st.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    st.errors.extend(error);
                }
            }
        }
        Reply::SearchDone { cancelled, .. } => {
            if let TabKind::Search(st) = &mut t.kind {
                st.running = false;
                if cancelled {
                    *cx.notice = Notice::info("Search cancelled");
                }
            }
        }
        Reply::Fks { res, .. } => {
            if let TabKind::Diagram(d) = &mut t.kind {
                d.edges = Some(res.map_err(|e| e.to_string()));
                d.build(&s.tables, &s.schema_cols);
            }
        }
        Reply::IndexAdvice { res, .. } => {
            if let TabKind::Activity(st) = &mut t.kind {
                st.advice = Some(res);
            }
        }
        Reply::ServerInfo { res, .. } => {
            if let TabKind::Activity(st) = &mut t.kind {
                st.server = Some(res.map_err(|e| e.to_string()));
            }
        }
        Reply::Preview { res, .. } => {
            if let Some(p) = t.editor_mut().and_then(|ed| ed.preview.as_mut()) {
                p.data = Some(res.map_err(|e| e.to_string()));
            }
        }
        Reply::Compared { res, .. } => {
            if let TabKind::Compare(st) = &mut t.kind {
                st.running = false;
                st.result = Some(res);
            }
        }
        Reply::DumpProgress { progress, .. } => {
            if let TabKind::Dump(st) = &mut t.kind {
                st.progress = progress;
            }
        }
        Reply::DumpDone { progress, res, .. } => {
            if let TabKind::Dump(st) = &mut t.kind {
                st.running = false;
                if res.is_ok() {
                    st.progress = progress;
                }
                st.took = st.started.map(|at| at.elapsed());
                st.outcome = Some(res);
            }
        }
        Reply::FkOptions { res, search, .. } => {
            // An older search answered after a newer one was sent: wait for that one.
            if let Some(p) = &mut t.fk_picker
                && (p.local || search == p.asked.trim())
            {
                p.loading = false;
                p.options = Some(res.map(fk_options).map_err(|e| e.to_string()));
            }
        }
        Reply::Distinct { res, .. } => {
            if let TabKind::Table(ts) = &mut t.kind
                && let Some(vf) = &mut ts.value_filter
            {
                vf.values = Some(
                    res.map(|r| {
                        vf.complete = r.rows.len() <= sql::DISTINCT_LIMIT;
                        r.rows
                            .into_iter()
                            .take(sql::DISTINCT_LIMIT)
                            .map(|row| {
                                let mut it = row.into_iter();
                                let v = it.next().flatten();
                                let n = it.next().flatten().and_then(|n| n.parse().ok()).unwrap_or(0);
                                (v, n)
                            })
                            .collect()
                    })
                    .map_err(|e| e.to_string()),
                );
            }
        }
        Reply::ColumnStats { res, .. } => {
            if let Some(v) = &mut t.col_stats {
                v.data = Some(
                    res.map_err(|e| e.to_string())
                        .and_then(|(a, b)| values::stats_from_queries(&a, &b)),
                );
            }
        }
        Reply::ScriptProgress { progress, .. } => {
            if let TabKind::Script(st) = &mut t.kind {
                st.progress = progress;
            }
        }
        Reply::ScriptDone {
            progress,
            error,
            slowest,
            ..
        } => {
            if let TabKind::Script(st) = &mut t.kind {
                st.running = false;
                st.progress = progress;
                st.slowest = slowest;
                st.took = st.started.map(|at| at.elapsed());
                st.outcome = Some(error);
            }
        }
        Reply::Source { res, .. } => match res {
            Ok(code) => {
                if let Some(ed) = t.editor_mut() {
                    ed.text = code;
                }
            }
            Err(e) => t.error = Some(e.to_string()),
        },
        Reply::Rows { res, notice, .. } => on_rows(t, s, *res, notice, cx),
        Reply::Count { res, .. } => {
            if let TabKind::Table(ts) = &mut t.kind {
                ts.total = res.ok();
            }
        }
        Reply::Saved { res, .. } => {
            match *res {
                Ok(r) => {
                    if let Some(ts) = editable_mut(&mut t.kind) {
                        ts.changes.clear();
                    }
                    t.result = Some(r);
                    t.selected = None;
                    t.error = None;
                }
                // Nothing was saved: the changes stay, so the user can fix and retry.
                Err(e) => t.error = Some(e.to_string()),
            }
        }
        Reply::Imported { res, .. } => {
            match res {
                Ok(n) => {
                    *cx.notice = Notice::info(format!("{n} row(s) imported"));
                    if let TabKind::Table(ts) = &mut t.kind {
                        ts.import = None;
                    }
                    reload(t, s);
                    let id = t.id;
                    if let TabKind::Table(ts) = &mut t.kind {
                        request_count(id, ts, s);
                    }
                }
                // The dialog stays open to fix the mapping and retry.
                Err(e) => t.error = Some(format!("Import failed: {e}")),
            }
        }
        Reply::Plan { res, .. } => match res {
            Ok(p) => {
                if let Some(ed) = t.editor_mut() {
                    ed.plan = Some(p);
                }
            }
            Err(e) => t.error = Some(e.to_string()),
        },
        Reply::Structure { res, .. } => match (res, &mut t.kind) {
            (Ok(d), TabKind::Structure(st)) => st.data = Some(d),
            (Err(e), _) => t.error = Some(e.to_string()),
            _ => {}
        },
        Reply::Exported { res, .. } => match res {
            Ok(done) => t.exported = Some(done),
            Err(e) => t.error = Some(format!("Export failed: {e}")),
        },
        Reply::Columns { res, incoming, .. } => {
            match (res, &mut t.kind) {
                (Ok(c), TabKind::Table(ts)) => {
                    ts.cols = c;
                    ts.incoming = incoming;
                }
                (Ok(c), TabKind::Sql(ed)) => {
                    if let Some(src) = &mut ed.source {
                        src.cols = c;
                        src.incoming = incoming;
                    }
                }
                (Err(e), _) => t.error = Some(e.to_string()),
                _ => {}
            }
            // First page waits for the columns so it can be ordered by the primary key.
            let first_load = matches!(&t.kind, TabKind::Table(ts) if ts.counted_filter.is_none());
            if first_load && t.result.is_none() {
                reload(t, s);
            }
        }
        // `Reply::tab` sends the connection's own replies to `on_session_reply`.
        _ => {}
    }
}

/// Rows of `SELECT key, label`: the choices of a foreign key picker.
fn fk_options(r: QueryResult) -> Vec<(String, Option<String>)> {
    r.rows
        .into_iter()
        .map(|row| {
            let mut it = row.into_iter();
            (it.next().flatten().unwrap_or_default(), it.next().flatten())
        })
        .collect()
}

/// A statement's rows (or error) for an editor, table or activity tab.
fn on_rows(
    t: &mut Tab,
    s: &Session,
    res: DbResult<QueryResult>,
    notice: Option<String>,
    cx: &mut TabReplyCx,
) {
    let ran = t.pending_sql.take();
    if let (TabKind::Sql(_), Some(sql), Ok(r)) = (&t.kind, &ran, &res)
        && r.previous.is_empty()
        && !r.rows.is_empty()
    {
        make_editable(t, s, sql);
    }
    let spot = match (&res, &ran, t.editor()) {
        (Err(e), Some(sql), Some(ed)) => locate_error(&ed.text, sql, e),
        _ => None,
    };
    if let Some(sql) = ran {
        let (ok, ms) = match &res {
            Ok(r) => (true, u64::try_from(r.elapsed.as_millis()).unwrap_or(u64::MAX)),
            Err(_) => (false, 0),
        };
        let entry = Entry {
            conn: s.cfg.name.clone(),
            sql,
            at: history::now(),
            ok,
            ms,
        };
        history::push(cx.history, entry);
    }
    t.selected = kept_activity_selection(t, &res);
    t.anchor = None;
    match res {
        Ok(mut r) => {
            let earlier = std::mem::take(&mut r.previous);
            t.timings = std::mem::take(&mut r.timings);
            t.sets.clear();
            if !earlier.is_empty() {
                t.sets = earlier;
                t.sets.push(QueryResult::default());
                t.set_shown = t.sets.len() - 1;
            }
            t.result = Some(r);
            if let TabKind::Design(d) = &t.kind {
                cx.designed
                    .push((t.id, d.schema.trim().to_string(), d.table.trim().to_string()));
            }
            match &mut t.kind {
                // Refreshes every 2 s must not wipe the outcome of a kill.
                TabKind::Activity(st) => {
                    t.error = None;
                    if notice.is_some() {
                        st.kill_error = notice;
                    }
                }
                _ => t.error = notice,
            }
        }
        Err(e) => {
            let mut message = e.to_string();
            if let Some(sp) = &spot {
                let _ = write!(message, " (line {}, column {})", sp.line, sp.column);
            }
            if let Some(ed) = t.editor_mut() {
                ed.error_jump = spot.is_some();
                ed.error_at = spot.map(|sp| (sp.start, sp.end));
            }
            t.error = Some(message);
        }
    }
}

/// A plain SELECT of one table from the editor: its rows can be edited in place once the
/// table's columns arrive.
fn make_editable(t: &mut Tab, s: &Session, sql: &str) {
    let Some((schema, table)) =
        sql::editable_select(sql).and_then(|(schema, table)| resolve_table(s, schema.as_deref(), &table))
    else {
        return;
    };
    let Some(ed) = t.editor_mut() else { return };
    ed.source = Some(Box::new(TableState::new(&schema, &table, None)));
    ed.source_query = sql.to_string();
    t.begin_job();
    s.send(Job::Columns {
        tab: t.id,
        schema,
        table,
    });
}

/// The activity list refreshes every few seconds: keep the selected session selected (by id)
/// instead of dropping the selection each time.
fn kept_activity_selection(t: &Tab, res: &DbResult<QueryResult>) -> Option<(usize, usize)> {
    let (TabKind::Activity(_), Some((row, col)), Some(old), Ok(new)) = (&t.kind, t.selected, &t.result, res)
    else {
        return None;
    };
    let id = old.rows.get(row).and_then(|r| r.first())?;
    new.rows
        .iter()
        .position(|r| r.first() == Some(id))
        .map(|r| (r, col))
}
