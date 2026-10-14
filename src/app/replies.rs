//! Replies from the connection's worker thread, applied to the session and the tabs.

use super::*;

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
                        self.notice = Notice::error("A conexão foi encerrada inesperadamente.");
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
                    Ok(()) if commit => Notice::info("Transação confirmada"),
                    Ok(()) => Notice::info("Transação desfeita"),
                    Err(e) => Notice::error(format!("Transação: {e}")),
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
                self.notice = Notice::info(format!("Conectado a {}", s.cfg.name));
            }
            Reply::Connected(Err(e)) => {
                self.notice = Notice::error(format!("Falha ao conectar: {e}"));
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
                Err(e) => self.notice = Notice::error(format!("Não foi possível: {e}")),
            },
            Reply::Location(Ok((dbs, db, schema))) => {
                // A switch (not the first report at connect) focuses the new one.
                let (before, now) = match s.cfg.kind {
                    Kind::Postgres => (&s.current_schema, &schema),
                    Kind::MySql => (&s.current_db, &db),
                };
                if !before.is_empty() && before != now {
                    self.schema_focus = Some(now.clone());
                }
                s.databases = dbs;
                s.current_db = db;
                s.current_schema = schema;
            }
            Reply::Location(Err(e)) => self.notice = Notice::error(format!("Não foi possível trocar: {e}")),
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
                    *cx.notice = Notice::info("Busca cancelada");
                }
            }
        }
        Reply::Fks { res, .. } => {
            if let TabKind::Diagram(d) = &mut t.kind {
                d.edges = Some(res.map_err(|e| e.to_string()));
                d.build(&s.tables, &s.schema_cols);
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
        Reply::ScriptDone { progress, error, .. } => {
            if let TabKind::Script(st) = &mut t.kind {
                st.running = false;
                st.progress = progress;
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
        Reply::Rows { res, notice, .. } => {
            let ran = t.pending_sql.take();
            // A plain SELECT of one table from the editor: its rows can be edited in place.
            let source = match (&t.kind, &ran, &*res) {
                (TabKind::Sql(_), Some(sql), Ok(r)) if r.previous.is_empty() && !r.rows.is_empty() => {
                    sql::editable_select(sql).and_then(|(schema, table)| {
                        resolve_table(s, schema.as_deref(), &table).map(|(sc, tb)| (sc, tb, sql.clone()))
                    })
                }
                _ => None,
            };
            if let Some((schema, table, query)) = source
                && let Some(ed) = t.editor_mut()
            {
                ed.source = Some(Box::new(TableState::new(&schema, &table, None)));
                ed.source_query = query;
                t.begin_job();
                s.send(Job::Columns {
                    tab: t.id,
                    schema,
                    table,
                });
            }
            let spot = match (&*res, &ran, t.editor()) {
                (Err(e), Some(sql), Some(ed)) => locate_error(&ed.text, sql, e),
                _ => None,
            };
            if let Some(sql) = ran {
                let (ok, ms) = match &*res {
                    Ok(r) => (true, r.elapsed.as_millis() as u64),
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
            // The activity list refreshes every few seconds: keep the selected session
            // selected (by id) instead of dropping the selection each time.
            let keep = match (&t.kind, t.selected, &t.result, &*res) {
                (TabKind::Activity(_), Some((row, col)), Some(old), Ok(new)) => {
                    let id = old.rows.get(row).and_then(|r| r.first()).cloned();
                    new.rows
                        .iter()
                        .position(|r| r.first() == id.as_ref())
                        .map(|r| (r, col))
                }
                _ => None,
            };
            t.selected = keep;
            t.anchor = None;
            match *res {
                Ok(mut r) => {
                    let earlier = std::mem::take(&mut r.previous);
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
                        message.push_str(&format!(" (linha {}, coluna {})", sp.line, sp.column));
                    }
                    if let Some(ed) = t.editor_mut() {
                        ed.error_jump = spot.is_some();
                        ed.error_at = spot.map(|sp| (sp.start, sp.end));
                    }
                    t.error = Some(message);
                }
            }
        }
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
                    *cx.notice = Notice::info(format!("{n} linha(s) importada(s)"));
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
                Err(e) => t.error = Some(format!("Importação falhou: {e}")),
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
            Err(e) => t.error = Some(format!("Exportação falhou: {e}")),
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
