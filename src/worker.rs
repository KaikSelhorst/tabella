use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::search::{SearchHit, SearchTarget};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use std::path::PathBuf;

use crate::db::{
    ActivityView, Canceller, ColumnInfo, Conn, ConnConfig, DbObject, FkEdge, IncomingFk, PreviewData,
    QueryResult, SchemaColumn, ServerInfo, TableRef, TableStructure,
};
use crate::error::{DbError, DbResult};
use crate::export::{Format, Writer};
use crate::import::ImportSpec;
use crate::plan::Plan;
use crate::sql::{self, PageQuery, Stmt, select_range};
use crate::tunnel::Tunnel;

pub enum Job {
    Query {
        tab: u64,
        sql: String,
    },
    /// Runs `stmt`, then `refresh`, and reports the rows of `refresh`. With `expect_one`, a
    /// statement that touched no row (it was changed or deleted elsewhere) is reported as a notice.
    Exec {
        tab: u64,
        stmt: Stmt,
        refresh: String,
        expect_one: bool,
    },
    Columns {
        tab: u64,
        schema: String,
        table: String,
    },
    Count {
        tab: u64,
        sql: String,
    },
    Tables,
    /// Pending grid edits, saved in one transaction; then `refresh` is run.
    Batch {
        tab: u64,
        stmts: Vec<Stmt>,
        refresh: String,
    },
    Structure {
        tab: u64,
        schema: String,
        table: String,
    },
    /// CSV file into a table, all or nothing.
    Import {
        tab: u64,
        spec: Box<ImportSpec>,
    },
    /// Query plan of one statement; `analyze` runs it to get real times.
    Explain {
        tab: u64,
        sql: String,
        analyze: bool,
    },
    /// Sessions on the server (answers with `Reply::Rows`).
    Activity {
        tab: u64,
        view: ActivityView,
    },
    /// A value in every table, one table after another, reporting as it goes; stops early
    /// when `cancel` is set.
    SearchValue {
        tab: u64,
        term: String,
        exact: bool,
        targets: Vec<SearchTarget>,
        cancel: Arc<AtomicBool>,
    },
    /// Every foreign key, for the ER diagram.
    Fks {
        tab: u64,
    },
    /// The code of a function, trigger, view…
    Source {
        tab: u64,
        object: DbObject,
    },
    /// The "Servidor" panel.
    ServerInfo {
        tab: u64,
    },
    /// Clear the "consultas lentas" counters, then show them again.
    ResetQueryStats {
        tab: u64,
    },
    /// Cancel another session's statement (or close it), then list sessions again.
    Kill {
        tab: u64,
        id: i64,
        terminate: bool,
        view: ActivityView,
    },
    Export {
        tab: u64,
        spec: ExportSpec,
    },
    /// What an UPDATE/DELETE would touch, without keeping any change.
    Preview {
        tab: u64,
        sql: String,
    },
    /// The two queries of `sql::column_stats`.
    ColumnStats {
        tab: u64,
        summary: String,
        top: String,
    },
    /// "Commit manual": writes open a transaction that only ends with `EndTx`.
    SetManual(bool),
    EndTx {
        commit: bool,
    },
}

/// Transaction state of the connection, reported with `Reply::Tx` whenever it changes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TxState {
    pub manual: bool,
    pub open: bool,
    /// Writing statements run since the transaction opened.
    pub writes: u32,
    /// A statement failed inside it (Postgres then refuses anything but ROLLBACK).
    pub failed: bool,
}

/// Rows per query when exporting a whole table.
const EXPORT_CHUNK: usize = 5000;

pub struct ExportSpec {
    pub format: Format,
    pub path: PathBuf,
    /// Qualified table name for SQL INSERTs.
    pub table_name: String,
    pub cols: Vec<ColumnInfo>,
    pub source: ExportSource,
}

pub enum ExportSource {
    /// Rows already on screen.
    Rows(QueryResult),
    /// The whole table (with the tab's filter and order), read in chunks.
    Table {
        schema: String,
        table: String,
        filter: String,
        order: Option<(String, bool)>,
        pk: Vec<String>,
    },
}

pub struct Connected {
    pub tables: Vec<TableRef>,
    pub canceller: Arc<Canceller>,
}

pub enum Reply {
    Connected(DbResult<Connected>),
    Tables(DbResult<Vec<TableRef>>),
    Rows {
        tab: u64,
        res: Box<DbResult<QueryResult>>,
        /// Something the user should know even though the query worked.
        notice: Option<String>,
    },
    Columns {
        tab: u64,
        res: DbResult<Vec<ColumnInfo>>,
        /// Foreign keys elsewhere pointing at this table (empty if they couldn't be read).
        incoming: Vec<IncomingFk>,
    },
    Count {
        tab: u64,
        res: DbResult<i64>,
    },
    /// `(table, column)` pairs for autocomplete; sent after connecting and on table reloads.
    AllColumns(DbResult<Vec<SchemaColumn>>),
    /// Every foreign key, for JOIN suggestions; sent with `AllColumns`.
    AllFks(DbResult<Vec<FkEdge>>),
    /// Functions, procedures, triggers, views and sequences, for the sidebar.
    Objects(DbResult<Vec<DbObject>>),
    /// Definition of an object, for the editor tab opened to show it.
    Source {
        tab: u64,
        res: DbResult<String>,
    },
    Structure {
        tab: u64,
        res: DbResult<TableStructure>,
    },
    Plan {
        tab: u64,
        res: DbResult<Plan>,
    },
    /// Rows inserted by `Job::Import`.
    Imported {
        tab: u64,
        res: DbResult<u64>,
    },
    /// Result of `Job::Batch`: the refreshed page, or why nothing was saved.
    Saved {
        tab: u64,
        res: Box<DbResult<QueryResult>>,
    },
    /// File written and how many rows went into it.
    Exported {
        tab: u64,
        res: DbResult<(PathBuf, u64)>,
    },
    ColumnStats {
        tab: u64,
        res: DbResult<(QueryResult, QueryResult)>,
    },
    Preview {
        tab: u64,
        res: DbResult<PreviewData>,
    },
    ServerInfo {
        tab: u64,
        res: DbResult<ServerInfo>,
    },
    /// One table of a `SearchValue` done: where it was found, or why that table failed.
    SearchProgress {
        tab: u64,
        hit: Option<SearchHit>,
        error: Option<String>,
    },
    SearchDone {
        tab: u64,
        cancelled: bool,
    },
    Fks {
        tab: u64,
        res: DbResult<Vec<FkEdge>>,
    },
    Tx(TxState),
    /// Outcome of `Job::EndTx`.
    TxEnded {
        commit: bool,
        res: DbResult<()>,
    },
}

fn send_tx(send: &impl Fn(Reply), before: TxState, now: TxState) {
    if before != now {
        send(Reply::Tx(now));
    }
}

/// Spawns a thread that owns the connection. Dropping the returned sender closes it once the
/// current statement (if any) finishes; use the `Canceller` to stop that statement early.
pub fn spawn(cfg: ConnConfig, ctx: eframe::egui::Context) -> (Sender<Job>, Receiver<Reply>) {
    let (job_tx, job_rx) = channel::<Job>();
    let (reply_tx, reply_rx) = channel::<Reply>();
    thread::spawn(move || {
        let send = |r: Reply| {
            let _ = reply_tx.send(r);
            ctx.request_repaint();
        };
        // Lives as long as this thread: dropping it stops ssh.
        let tunnel = match &cfg.ssh {
            Some(ssh) => match Tunnel::open(ssh, &cfg.host, cfg.port) {
                Ok(t) => Some(t),
                Err(e) => return send(Reply::Connected(Err(DbError::Other(e)))),
            },
            None => None,
        };
        let via = tunnel.as_ref().map(|t| t.local_port);
        let mut conn = match Conn::connect(&cfg, via) {
            Ok(c) => c,
            Err(e) => return send(Reply::Connected(Err(e))),
        };
        let canceller = Arc::new(conn.canceller(&cfg, via));
        send(Reply::Connected(
            conn.tables().map(|tables| Connected { tables, canceller }),
        ));
        send(Reply::AllColumns(conn.all_columns()));
        send(Reply::AllFks(conn.all_fks()));
        send(Reply::Objects(conn.objects()));
        let mut tx = TxState {
            manual: cfg.manual_commit,
            ..Default::default()
        };
        while let Ok(job) = job_rx.recv() {
            let before = tx;
            let pg = cfg.kind == crate::db::Kind::Postgres;
            // In manual mode, the first write opens the transaction.
            let open_for_write = |conn: &mut Conn, tx: &mut TxState| -> DbResult<()> {
                if tx.manual && !tx.open {
                    conn.begin()?;
                    tx.open = true;
                }
                Ok(())
            };
            // Bookkeeping after a user statement ran inside the transaction.
            let note = |tx: &mut TxState, ok: bool, writes: u32| {
                if tx.open {
                    if ok {
                        tx.writes += writes;
                    } else if pg {
                        tx.failed = true;
                    }
                }
            };
            let reply = match job {
                Job::Query { tab, sql } => {
                    let control = sql::tx_control(&sql);
                    let writes = control.is_none() && sql::script_risk(&sql).writes;
                    let res = if writes {
                        open_for_write(&mut conn, &mut tx).and_then(|()| conn.query(&sql))
                    } else {
                        conn.query(&sql)
                    };
                    let implicit_commit = !pg && res.is_ok() && sql::mysql_implicit_commit(&sql);
                    match control {
                        // MySQL committed everything before running DDL.
                        None if implicit_commit => {
                            tx = TxState {
                                manual: tx.manual,
                                ..Default::default()
                            }
                        }
                        // The user ended or started it by hand.
                        Some(c) if res.is_ok() => {
                            tx = TxState {
                                manual: tx.manual,
                                open: c == sql::TxControl::Begin,
                                ..Default::default()
                            }
                        }
                        _ => note(
                            &mut tx,
                            res.is_ok(),
                            if writes {
                                sql::statement_ranges(&sql, false).len() as u32
                            } else {
                                0
                            },
                        ),
                    }
                    Reply::Rows {
                        tab,
                        res: Box::new(res),
                        notice: None,
                    }
                }
                Job::Preview { tab, sql } => Reply::Preview {
                    tab,
                    res: conn.preview(&sql, tx.open),
                },
                Job::ColumnStats { tab, summary, top } => Reply::ColumnStats {
                    tab,
                    res: conn.query(&summary).and_then(|a| Ok((a, conn.query(&top)?))),
                },
                Job::SetManual(on) => {
                    tx.manual = on;
                    send_tx(&send, before, tx);
                    continue;
                }
                Job::EndTx { commit } => {
                    let res = if commit && tx.failed {
                        let _ = conn.query("ROLLBACK");
                        Err(DbError::Other(
                            "um comando da transação falhou, então ela foi desfeita: nada foi confirmado"
                                .into(),
                        ))
                    } else {
                        conn.query(if commit { "COMMIT" } else { "ROLLBACK" }).map(|_| ())
                    };
                    tx = TxState {
                        manual: tx.manual,
                        ..Default::default()
                    };
                    Reply::TxEnded { commit, res }
                }
                Job::Exec {
                    tab,
                    stmt,
                    refresh,
                    expect_one,
                } => {
                    let mut notice = None;
                    let ran = open_for_write(&mut conn, &mut tx).and_then(|()| conn.execute(&stmt));
                    note(&mut tx, ran.is_ok(), 1);
                    let res = ran.and_then(|done| {
                        if expect_one && done.affected == Some(0) {
                            notice = Some(
                                "Nenhuma linha foi alterada: ela mudou ou foi excluída em outra sessão. \
                                 A página foi recarregada."
                                    .to_string(),
                            );
                        }
                        conn.query(&refresh)
                    });
                    Reply::Rows {
                        tab,
                        res: Box::new(res),
                        notice,
                    }
                }
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
                    let res =
                        open_for_write(&mut conn, &mut tx).and_then(|()| conn.run_batch(&stmts, tx.open));
                    // A failed batch is rolled back to its savepoint: the transaction stays usable.
                    if res.is_ok() {
                        note(&mut tx, true, stmts.len() as u32);
                    }
                    Reply::Saved {
                        tab,
                        res: Box::new(res.and_then(|()| conn.query(&refresh))),
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
                } => {
                    for t in &targets {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let (hit, error) =
                            match crate::search::search_table(&mut conn, cfg.kind, t, &term, exact) {
                                Ok(h) => (h, None),
                                Err(e) => (None, Some(format!("{}.{}: {e}", t.schema, t.table))),
                            };
                        send(Reply::SearchProgress { tab, hit, error });
                    }
                    Reply::SearchDone {
                        tab,
                        cancelled: cancel.load(Ordering::Relaxed),
                    }
                }
                Job::Source { tab, object } => Reply::Source {
                    tab,
                    res: conn.object_source(&object),
                },
                Job::Fks { tab } => Reply::Fks {
                    tab,
                    res: conn.all_fks(),
                },
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
                    let res = open_for_write(&mut conn, &mut tx)
                        .and_then(|()| crate::import::run(&mut conn, &spec, tx.open));
                    if let Ok(n) = &res {
                        note(&mut tx, true, *n as u32);
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
                    res: export(&mut conn, spec),
                },
            };
            send(reply);
            send_tx(&send, before, tx);
        }
    });
    (job_tx, reply_rx)
}

fn export(conn: &mut Conn, spec: ExportSpec) -> DbResult<(PathBuf, u64)> {
    let io = |e: std::io::Error| DbError::Other(format!("erro ao gravar {}: {e}", spec.path.display()));
    let kind = conn.kind();
    let rows = match spec.source {
        ExportSource::Rows(res) => {
            let mut w = Writer::create(&spec.path, spec.format, kind, &spec.table_name, &res, &spec.cols)
                .map_err(io)?;
            w.rows(&res.rows).map_err(io)?;
            w.finish().map_err(io)?
        }
        ExportSource::Table {
            schema,
            table,
            filter,
            order,
            pk,
        } => {
            let q = PageQuery {
                schema: &schema,
                table: &table,
                filter: &filter,
                order: order.as_ref().map(|(c, asc)| (c.as_str(), *asc)),
                pk: &pk,
                page: 0,
            };
            // Ordered by the primary key, so chunks neither skip nor repeat rows.
            let mut writer: Option<Writer> = None;
            let mut offset = 0;
            loop {
                let chunk = conn.query(&select_range(kind, &q, EXPORT_CHUNK, offset))?;
                let w = match &mut writer {
                    Some(w) => w,
                    None => writer.insert(
                        Writer::create(
                            &spec.path,
                            spec.format,
                            kind,
                            &spec.table_name,
                            &chunk,
                            &spec.cols,
                        )
                        .map_err(io)?,
                    ),
                };
                w.rows(&chunk.rows).map_err(io)?;
                offset += chunk.rows.len();
                if chunk.rows.len() < EXPORT_CHUNK {
                    break;
                }
            }
            writer.map(Writer::finish).transpose().map_err(io)?.unwrap_or(0)
        }
    };
    Ok((spec.path, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Kind;

    /// Needs `docker compose --profile test up -d --wait`.
    fn export_whole_table(kind: Kind, port: u16, db: &str) {
        let mut cfg = ConnConfig::new(kind);
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        cfg.password = "pw".into();
        cfg.database = db.into();
        let mut c = Conn::connect(&cfg, None).expect("connect");
        let schema = if kind == Kind::MySql { db } else { "public" };
        c.query("DROP TABLE IF EXISTS dbl_export").unwrap();
        c.query("CREATE TABLE dbl_export (id INT PRIMARY KEY, v VARCHAR(20))")
            .unwrap();
        // 12 000 rows: more than two export chunks.
        let fill = match kind {
            Kind::Postgres => "INSERT INTO dbl_export SELECT g, 'v' || g FROM generate_series(1, 12000) g",
            Kind::MySql => {
                c.query("SET SESSION cte_max_recursion_depth = 20000").unwrap();
                "INSERT INTO dbl_export WITH RECURSIVE s (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 12000) \
                 SELECT n, CONCAT('v', n) FROM s"
            }
        };
        c.query(fill).unwrap();
        let path = std::env::temp_dir().join(format!("dblite-export-{kind:?}-{}.csv", std::process::id()));
        let cols = c.columns(schema, "dbl_export").unwrap();
        let spec = ExportSpec {
            format: Format::Csv,
            path: path.clone(),
            table_name: kind.qualified(schema, "dbl_export"),
            cols,
            source: ExportSource::Table {
                schema: schema.into(),
                table: "dbl_export".into(),
                filter: "id > 10".into(),
                order: None,
                pk: vec!["id".into()],
            },
        };
        let (_, rows) = export(&mut c, spec).unwrap();
        assert_eq!(rows, 11_990, "{kind:?}");
        let text = std::fs::read_to_string(&path).unwrap();
        let ids: Vec<u32> = text
            .lines()
            .skip(1)
            .map(|l| l.split(',').next().unwrap().parse().unwrap())
            .collect();
        // Every row once, in key order: no gaps or repeats at chunk boundaries.
        assert_eq!(ids, (11..=12000).collect::<Vec<_>>(), "{kind:?}");
        let _ = std::fs::remove_file(&path);

        // The "new row" form's INSERT: only filled columns, values as parameters.
        let values = vec![
            ("id".to_string(), Some("20000".to_string())),
            ("v".to_string(), Some("it's".to_string())),
        ];
        let stmt = crate::sql::insert_values(kind, schema, "dbl_export", &values);
        assert_eq!(c.execute(&stmt).unwrap().affected, Some(1));
        let r = c.query("SELECT v FROM dbl_export WHERE id = 20000").unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("it's"));
        c.query("DROP TABLE dbl_export").unwrap();
    }

    /// Manual commit through the worker: writes stay invisible to other sessions until
    /// confirmed, undo reverts SQL and grid edits alike, and a failed grid batch doesn't spoil
    /// the transaction.
    fn manual_commit(kind: Kind, port: u16, db: &str) {
        use std::time::Duration;
        let mut cfg = ConnConfig::new(kind);
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        cfg.password = "pw".into();
        cfg.database = db.into();
        let mut other = Conn::connect(&cfg, None).expect("connect");
        other.query("DROP TABLE IF EXISTS dbl_tx").unwrap();
        other
            .query("CREATE TABLE dbl_tx (id INT PRIMARY KEY, v VARCHAR(20))")
            .unwrap();
        other.query("INSERT INTO dbl_tx VALUES (1, 'a')").unwrap();
        let seen = |other: &mut Conn| {
            other.query("SELECT v FROM dbl_tx WHERE id = 1").unwrap().rows[0][0]
                .clone()
                .unwrap()
        };

        cfg.manual_commit = true;
        let (jobs, replies) = spawn(cfg, eframe::egui::Context::default());
        // Next reply that `f` picks out, skipping the rest.
        let wait = |f: &dyn Fn(Reply) -> Option<String>| -> String {
            loop {
                let r = replies
                    .recv_timeout(Duration::from_secs(10))
                    .expect("worker reply");
                if let Some(v) = f(r) {
                    return v;
                }
            }
        };
        let tx_state = |r: Reply| match r {
            Reply::Tx(t) => Some(format!("{}:{}:{}", t.open, t.writes, t.failed)),
            _ => None,
        };
        let saved = |r: Reply| match r {
            Reply::Saved { res, .. } => Some(match *res {
                Ok(_) => "ok".to_string(),
                Err(e) => e.to_string(),
            }),
            _ => None,
        };
        let ended = |r: Reply| match r {
            Reply::TxEnded { res, .. } => {
                Some(res.map(|()| "ok".to_string()).unwrap_or_else(|e| e.to_string()))
            }
            _ => None,
        };
        let set = |v: &str, id: &str| {
            Stmt::new("UPDATE dbl_tx SET v = ")
                .value(Some(v))
                .sql(" WHERE id = ")
                .value(Some(id))
        };

        jobs.send(Job::Query {
            tab: 1,
            sql: "SELECT 1".into(),
        })
        .unwrap();
        jobs.send(Job::Query {
            tab: 1,
            sql: "UPDATE dbl_tx SET v = 'b' WHERE id = 1".into(),
        })
        .unwrap();
        assert_eq!(wait(&tx_state), "true:1:false", "{kind:?}: a write opens it");
        assert_eq!(seen(&mut other), "a", "{kind:?}: not visible before commit");
        jobs.send(Job::Batch {
            tab: 1,
            stmts: vec![set("c", "1")],
            refresh: "SELECT 1".into(),
        })
        .unwrap();
        assert_eq!(wait(&saved), "ok");
        jobs.send(Job::Batch {
            tab: 1,
            stmts: vec![set("x", "999")],
            refresh: "SELECT 1".into(),
        })
        .unwrap();
        assert!(wait(&saved).contains("Nada foi salvo"));
        jobs.send(Job::EndTx { commit: false }).unwrap();
        assert_eq!(wait(&ended), "ok", "{kind:?}");
        assert_eq!(seen(&mut other), "a", "{kind:?}: undo reverts SQL and grid edits");

        jobs.send(Job::Query {
            tab: 1,
            sql: "UPDATE dbl_tx SET v = 'd' WHERE id = 1".into(),
        })
        .unwrap();
        jobs.send(Job::EndTx { commit: true }).unwrap();
        assert_eq!(wait(&ended), "ok");
        assert_eq!(seen(&mut other), "d", "{kind:?}: committed");

        if kind == Kind::Postgres {
            // After an error Postgres only accepts ROLLBACK: "Confirmar" says so instead of
            // pretending it worked.
            assert_eq!(wait(&tx_state), "false:0:false", "closed after the commit");
            jobs.send(Job::Query {
                tab: 1,
                sql: "UPDATE dbl_tx SET v = 'e' WHERE id = 1".into(),
            })
            .unwrap();
            jobs.send(Job::Query {
                tab: 1,
                sql: "UPDATE dbl_tx SET nope = 1".into(),
            })
            .unwrap();
            assert_eq!(wait(&tx_state), "true:1:false");
            assert_eq!(wait(&tx_state), "true:1:true");
            jobs.send(Job::EndTx { commit: true }).unwrap();
            assert!(wait(&ended).contains("desfeita"));
            assert_eq!(seen(&mut other), "d");
        }
        drop(jobs);
        other.query("DROP TABLE dbl_tx").unwrap();
    }

    #[test]
    #[ignore]
    fn manual_commit_postgres() {
        manual_commit(Kind::Postgres, 55432, "postgres");
    }

    #[test]
    #[ignore]
    fn manual_commit_mysql() {
        manual_commit(Kind::MySql, 53306, "test");
    }

    #[test]
    #[ignore]
    fn export_postgres() {
        export_whole_table(Kind::Postgres, 55432, "postgres");
    }

    #[test]
    #[ignore]
    fn export_mysql() {
        export_whole_table(Kind::MySql, 53306, "test");
    }
}
