//! The loop of one connection: a job at a time, reconnecting, and transaction bookkeeping.

use super::*;

pub(super) fn send_tx(send: &impl Fn(Reply), before: TxState, now: TxState) {
    if before != now {
        send(Reply::Tx(now));
    }
}

/// The connection's thread: runs jobs one at a time and keeps the transaction's bookkeeping.
pub(super) struct Worker<'a, S: Fn(Reply)> {
    pub(super) conn: Conn,
    pub(super) tx: TxState,
    pub(super) pg: bool,
    pub(super) cfg: &'a ConnConfig,
    /// The SSH tunnel, if any, and the local port the connection went through.
    pub(super) link: Option<Arc<Link>>,
    pub(super) via: Option<u16>,
    /// The schema or database picked with `UseSchema`: session state a new connection lacks.
    pub(super) location: Option<String>,
    pub(super) last_job: Instant,
    /// For replies sent before the job's own (progress, refreshed lists).
    pub(super) send: &'a S,
    /// Read by `send`: the number of the job running now.
    pub(super) seq: &'a std::cell::Cell<u64>,
    /// The tab whose job runs now, and how to cancel it.
    pub(super) running: &'a Running,
    /// Tabs cancelled, whose jobs still in line are skipped.
    pub(super) cancels: &'a Cancels,
    /// The session's second connection: it only reads, and a lost connection is replaced
    /// without telling the app (no session state of the user's was on it).
    pub(super) background: bool,
}

/// MySQL isn't asked whether it is alive on every click, only after this long without a job.
pub(super) const PING_AFTER_IDLE: Duration = Duration::from_secs(30);
/// Tries to get a dropped connection back, and the wait before each.
pub(super) const RECONNECT_WAITS: [Duration; 3] =
    [Duration::ZERO, Duration::from_secs(1), Duration::from_secs(3)];

impl<S: Fn(Reply)> Worker<'_, S> {
    /// Runs one job and sends its reply. Returns false when the connection is gone and
    /// couldn't be replaced.
    pub(super) fn run(&mut self, q: Queued) -> bool {
        let Queued { job, taken, seq } = q;
        self.seq.set(seq);
        if job.skippable() && self.cancels.skips(job.tab(), taken) {
            if let Some(reply) = job.failed(|| DbError::Other("cancelled".into())) {
                (self.send)(reply);
            }
            return true;
        }
        let before = self.tx;
        let mut alive = self.ensure_alive(false);
        // Should its code panic, the tab still gets an answer and the app stays up.
        let crashed = job.failed(|| DbError::Other(crate::crash::internal_error()));
        self.running.start(job.tab());
        let reply = match crate::crash::guard(|| self.handle(job)) {
            Ok(reply) => reply,
            Err(_) => {
                // Whatever the connection was doing was cut halfway: it can't be trusted.
                self.running.set_tab(None);
                alive = self.reconnect();
                crashed
            }
        };
        // A job that found the connection dead (MySQL can't tell beforehand): it is replaced
        // below, so the run-again the user is about to do works.
        let lost = reply.as_ref().is_some_and(Reply::connection_lost);
        if let Some(reply) = reply {
            (self.send)(reply);
        }
        // Only after the reply: a cancel being sent holds this, and the reply needn't wait.
        self.running.set_tab(None);
        if lost {
            alive = self.ensure_alive(true);
        }
        self.last_job = Instant::now();
        send_tx(self.send, before, self.tx);
        self.seq.set(0);
        alive
    }

    /// A connection that dropped since the last job (idle timeout, restart, network) is replaced
    /// before the next job runs, so it doesn't fail with "connection closed". Whatever the old
    /// session held (an open transaction) is gone; the app is told.
    pub(super) fn ensure_alive(&mut self, ping: bool) -> bool {
        self.conn
            .is_alive(ping || self.last_job.elapsed() >= PING_AFTER_IDLE)
            || self.reconnect()
    }

    /// Replaces the connection with a new one (the old one's transaction goes with it, and the
    /// app is told). False when none could be made.
    pub(super) fn reconnect(&mut self) -> bool {
        let mut last_error = None;
        for wait in RECONNECT_WAITS {
            thread::sleep(wait);
            let name = if self.background {
                BACKGROUND_NAME
            } else {
                "tabella"
            };
            // A dead tunnel is opened again, on another port.
            let via = match route(self.link.as_ref()) {
                Ok(via) => via,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
            match Conn::connect_as(self.cfg, via, name) {
                Ok(mut conn) => {
                    self.via = via;
                    if let Some(name) = &self.location {
                        let _ = conn.use_schema(name);
                    }
                    self.running.set_canceller(conn.canceller(self.cfg, self.via));
                    let rolled_back = self.tx.open;
                    self.tx = TxState {
                        manual: self.tx.manual,
                        ..Default::default()
                    };
                    let res = conn.tables().map(|tables| Connected { tables });
                    self.conn = conn;
                    if !self.background {
                        (self.send)(Reply::Reconnected { res, rolled_back });
                    }
                    return true;
                }
                Err(e) => last_error = Some(e),
            }
        }
        if let Some(e) = last_error
            && !self.background
        {
            (self.send)(Reply::Reconnected {
                res: Err(e),
                rolled_back: false,
            });
        }
        false
    }

    /// Runs one job; `None` when it has no reply of its own.
    pub(super) fn handle(&mut self, job: Job) -> Option<Reply> {
        let conn = &mut self.conn;
        let send = self.send;
        Some(match job {
            Job::Query { tab, sql } => {
                // Tests: a job whose code hits a bug.
                #[cfg(test)]
                assert_ne!(sql, tests::PANIC, "a bug in a job");
                Reply::Rows {
                    tab,
                    res: Box::new(run_editor_sql(conn, &mut self.tx, self.pg, &sql)),
                    notice: None,
                }
            }
            Job::MoreRows { tab, sql, limit } => Reply::Rows {
                tab,
                res: Box::new(conn.query_up_to(&sql, limit)),
                notice: None,
            },
            Job::Preview { tab, sql } => Reply::Preview {
                tab,
                res: conn.preview(&sql, self.tx.open),
            },
            Job::Compare {
                tab,
                source_schema,
                target,
                target_schema,
            } => Reply::Compared {
                tab,
                res: compare_with(conn, &source_schema, target.as_deref(), &target_schema),
            },
            Job::Dump { tab, spec, cancel } => self.dump(tab, &spec, &cancel),
            Job::FkOptions { tab, sql, search } => Reply::FkOptions {
                tab,
                search,
                res: capped_read(conn, self.pg, self.tx.open, &sql),
            },
            Job::Distinct { tab, sql } => Reply::Distinct {
                tab,
                res: conn.query(&sql),
            },
            Job::ColumnStats { tab, summary, top } => Reply::ColumnStats {
                tab,
                res: conn.query(&summary).and_then(|a| Ok((a, conn.query(&top)?))),
            },
            Job::SetManual(on) => {
                self.tx.manual = on;
                return None;
            }
            Job::EndTx { commit } => self.end_tx(commit),
            Job::Exec {
                tab,
                stmt,
                refresh,
                expect_one,
            } => self.exec(tab, &stmt, &refresh, expect_one),
            Job::Columns { tab, schema, table } => Reply::Columns {
                tab,
                res: conn.columns_with_fks(&schema, &table),
                incoming: conn.incoming_fks(&schema, &table).unwrap_or_default(),
            },
            Job::Count { tab, sql } => Reply::Count {
                tab,
                res: conn.count(&sql),
            },
            Job::Tables => {
                let tables = conn.tables();
                send(Reply::AllColumns(conn.all_columns()));
                send(Reply::AllFks(conn.all_fks()));
                send(Reply::Objects(conn.objects()));
                Reply::Tables(tables)
            }
            Job::Batch { tab, stmts, refresh } => {
                let res = self
                    .open_for_write()
                    .and_then(|()| self.conn.run_batch(&stmts, self.tx.open));
                // A failed batch is rolled back to its savepoint: the transaction stays usable.
                if res.is_ok() {
                    self.note(true, stmts.len() as u64);
                }
                Reply::Saved {
                    tab,
                    res: Box::new(res.and_then(|()| self.conn.query(&refresh))),
                }
            }
            Job::Activity { tab, view } => Reply::Rows {
                tab,
                res: Box::new(conn.activity_view(view)),
                notice: None,
            },
            Job::SearchValue {
                tab,
                term,
                exact,
                targets,
                cancel,
            } => self.search(tab, &term, exact, &targets, &cancel),
            Job::Source { tab, object } => Reply::Source {
                tab,
                res: conn.object_source(&object),
            },
            Job::RunScript {
                tab,
                path,
                transaction,
                cancel,
            } => {
                let (progress, error, slowest) =
                    run_script(conn, &mut self.tx, self.pg, &path, transaction, &cancel, |p| {
                        send(Reply::ScriptProgress { tab, progress: p })
                    });
                // Scripts usually create or change tables.
                send(Reply::Tables(conn.tables()));
                send(Reply::AllColumns(conn.all_columns()));
                send(Reply::Objects(conn.objects()));
                Reply::ScriptDone {
                    tab,
                    progress,
                    error,
                    slowest,
                }
            }
            Job::Admin { sql, done, failed } => {
                let res = run_user_sql(conn, &mut self.tx, self.pg, &sql).map(|_| ());
                // The sidebar shows the change (a new, renamed or dropped table).
                if res.is_ok() {
                    send(Reply::Tables(conn.tables()));
                    send(Reply::AllColumns(conn.all_columns()));
                }
                Reply::Admin { done, failed, res }
            }
            Job::UseSchema(name) => {
                let res = conn.use_schema(&name).and_then(|()| conn.location());
                if res.is_ok() {
                    self.location = Some(name);
                }
                // The first connection answers for both.
                if self.background {
                    return None;
                }
                Reply::Location(res)
            }
            Job::Fks { tab } => Reply::Fks {
                tab,
                res: conn.all_fks(),
            },
            Job::SuggestIndex { tab, sql, schema } => {
                let kind = conn.kind();
                let res = crate::advisor::parse(&sql)
                    .and_then(|plans| {
                        crate::advisor::advise(kind, &schema, &plans, |s, t| conn.structure(s, t).ok())
                    })
                    .map_err(DbError::Other);
                Reply::IndexAdvice { tab, res }
            }
            Job::ServerInfo { tab } => Reply::ServerInfo {
                tab,
                res: conn.server_info(),
            },
            Job::ResetQueryStats { tab } => {
                let notice = conn.reset_query_stats().err().map(|e| e.to_string());
                Reply::Rows {
                    tab,
                    res: Box::new(conn.activity_view(ActivityView::Slow)),
                    notice,
                }
            }
            Job::Kill {
                tab,
                id,
                terminate,
                view,
            } => {
                // A refused kill still refreshes the list; the reason comes as the notice.
                let notice = conn.kill(id, terminate).err().map(|e| e.to_string());
                Reply::Rows {
                    tab,
                    res: Box::new(conn.activity_view(view)),
                    notice,
                }
            }
            Job::Import { tab, spec } => {
                let res = self
                    .open_for_write()
                    .and_then(|()| crate::import::run(&mut self.conn, &spec, self.tx.open));
                if let Ok(n) = &res {
                    self.note(true, *n);
                }
                Reply::Imported { tab, res }
            }
            Job::Explain { tab, sql, analyze } => Reply::Plan {
                tab,
                res: conn.explain(&sql, analyze),
            },
            Job::Structure { tab, schema, table } => Reply::Structure {
                tab,
                res: conn.structure(&schema, &table),
            },
            Job::Export { tab, spec } => Reply::Exported {
                tab,
                res: export(conn, spec),
            },
        })
    }

    /// In manual mode, the first write opens the transaction.
    pub(super) fn open_for_write(&mut self) -> DbResult<()> {
        if self.tx.manual && !self.tx.open {
            self.conn.begin()?;
            self.tx.open = true;
        }
        Ok(())
    }

    /// Bookkeeping after a user statement ran inside the transaction.
    pub(super) fn note(&mut self, ok: bool, writes: u64) {
        if self.tx.open {
            if ok {
                self.tx.writes = self.tx.writes.saturating_add(writes);
            } else if self.pg {
                self.tx.failed = true;
            }
        }
    }

    pub(super) fn end_tx(&mut self, commit: bool) -> Reply {
        let res = if commit && self.tx.failed {
            // Postgres refuses COMMIT after a failed statement anyway; say why nothing was kept.
            let rolled_back = self.conn.query("ROLLBACK");
            Err(DbError::Other(match rolled_back {
                Ok(_) => {
                    "a statement in the transaction failed, so it was rolled back: nothing was committed"
                        .into()
                }
                Err(e) => format!("a statement in the transaction failed and so did the ROLLBACK: {e}"),
            }))
        } else {
            self.conn
                .query(if commit { "COMMIT" } else { "ROLLBACK" })
                .map(|_| ())
        };
        self.tx = TxState {
            manual: self.tx.manual,
            ..Default::default()
        };
        Reply::TxEnded { commit, res }
    }

    /// One edit from the grid, then the page again.
    pub(super) fn exec(&mut self, tab: u64, stmt: &Stmt, refresh: &str, expect_one: bool) -> Reply {
        let mut notice = None;
        let ran = self.open_for_write().and_then(|()| self.conn.execute(stmt));
        self.note(ran.is_ok(), 1);
        let res = ran.and_then(|done| {
            if expect_one && done.affected == Some(0) {
                notice = Some(
                    "No rows were changed: the row changed or was deleted in another session. \
                     The page was reloaded."
                        .to_string(),
                );
            }
            self.conn.query(refresh)
        });
        Reply::Rows {
            tab,
            res: Box::new(res),
            notice,
        }
    }

    pub(super) fn dump(&mut self, tab: u64, spec: &crate::dump::DumpSpec, cancel: &AtomicBool) -> Reply {
        let send = self.send;
        let last = std::cell::Cell::new(std::time::Instant::now());
        let res = crate::dump::dump(&mut self.conn, spec, cancel, |progress| {
            // A few times a second is plenty for a progress bar.
            if last.get().elapsed() >= std::time::Duration::from_millis(150) {
                last.set(std::time::Instant::now());
                send(Reply::DumpProgress { tab, progress });
            }
        });
        match res {
            Ok(progress) => Reply::DumpDone {
                tab,
                progress,
                res: Ok(spec.path.clone()),
            },
            Err(e) => Reply::DumpDone {
                tab,
                progress: Default::default(),
                res: Err(if cancel.load(Ordering::Relaxed) {
                    DbError::Other("cancelled".into())
                } else {
                    e
                }),
            },
        }
    }

    /// Looks for a value table by table, reporting each as it finishes.
    pub(super) fn search(
        &mut self,
        tab: u64,
        term: &str,
        exact: bool,
        targets: &[crate::search::SearchTarget],
        cancel: &AtomicBool,
    ) -> Reply {
        let mut lost = false;
        for t in targets {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let (hit, error) =
                match crate::search::search_table(&mut self.conn, self.cfg.kind, t, term, exact) {
                    Ok(h) => (h, None),
                    Err(e) => {
                        // Every table after it would fail the same way.
                        lost = e.is_connection_lost();
                        (None, Some(format!("{}.{}: {e}", t.schema, t.table)))
                    }
                };
            (self.send)(Reply::SearchProgress { tab, hit, error });
            if lost {
                break;
            }
        }
        Reply::SearchDone {
            tab,
            cancelled: cancel.load(Ordering::Relaxed),
            lost,
            error: None,
        }
    }
}
