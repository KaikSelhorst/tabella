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
    /// Where unqualified names go (USE / search_path); answers with `Reply::Location`.
    UseSchema(String),
    /// A `.sql` file run statement by statement, reporting progress; stops at the first error
    /// (or when `cancel` is set). With `transaction`, all of it or nothing.
    RunScript {
        tab: u64,
        path: PathBuf,
        transaction: bool,
        cancel: Arc<AtomicBool>,
    },
    /// A table action (rename, duplicate, truncate, drop) confirmed in its dialog.
    Admin {
        sql: String,
        /// What to say when it's done, e.g. "clientes renomeada".
        done: String,
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
    /// "Filtrar valores": a column's distinct values with their counts.
    Distinct {
        tab: u64,
        sql: String,
    },
    /// "Comparar estrutura": `target: None` is another schema of this connection.
    Compare {
        tab: u64,
        source_schema: String,
        target: Option<Box<ConnConfig>>,
        target_schema: String,
    },
    /// "Exportar .sql".
    Dump {
        tab: u64,
        spec: crate::dump::DumpSpec,
        cancel: Arc<AtomicBool>,
    },
    /// "Commit manual": writes open a transaction that only ends with `EndTx`.
    SetManual(bool),
    EndTx {
        commit: bool,
    },
}

/// How far a script file got.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScriptProgress {
    pub statements: u64,
    /// Rows loaded by COPY blocks.
    pub copied: u64,
    /// psql meta-commands left out.
    pub skipped: u64,
    pub bytes: u64,
    pub line: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScriptError {
    pub message: String,
    /// Line where the failing statement starts, and its beginning.
    pub line: u64,
    pub statement: String,
    pub cancelled: bool,
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
    /// `(databases, current database, current schema)`.
    Location(DbResult<(Vec<String>, String, String)>),
    Admin {
        done: String,
        res: DbResult<()>,
    },
    ScriptProgress {
        tab: u64,
        progress: ScriptProgress,
    },
    ScriptDone {
        tab: u64,
        progress: ScriptProgress,
        /// `None` when every statement ran.
        error: Option<ScriptError>,
    },
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
    Distinct {
        tab: u64,
        res: DbResult<QueryResult>,
    },
    DumpProgress {
        tab: u64,
        progress: crate::dump::DumpProgress,
    },
    Compared {
        tab: u64,
        res: Result<Vec<crate::compare::TableDiff>, String>,
    },
    DumpDone {
        tab: u64,
        progress: crate::dump::DumpProgress,
        res: Result<PathBuf, String>,
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

impl Reply {
    /// The tab a reply answers and whether it ends that tab's job (progress reports and the
    /// row count, which runs beside the page, don't); `None` for the connection's own.
    pub fn tab(&self) -> Option<(u64, bool)> {
        match self {
            Reply::Rows { tab, .. }
            | Reply::Columns { tab, .. }
            | Reply::ScriptDone { tab, .. }
            | Reply::Source { tab, .. }
            | Reply::Structure { tab, .. }
            | Reply::Plan { tab, .. }
            | Reply::Imported { tab, .. }
            | Reply::Saved { tab, .. }
            | Reply::Exported { tab, .. }
            | Reply::ColumnStats { tab, .. }
            | Reply::Distinct { tab, .. }
            | Reply::DumpDone { tab, .. }
            | Reply::Compared { tab, .. }
            | Reply::Preview { tab, .. }
            | Reply::ServerInfo { tab, .. }
            | Reply::SearchDone { tab, .. }
            | Reply::Fks { tab, .. } => Some((*tab, true)),
            Reply::Count { tab, .. }
            | Reply::ScriptProgress { tab, .. }
            | Reply::SearchProgress { tab, .. }
            | Reply::DumpProgress { tab, .. } => Some((*tab, false)),
            Reply::Connected(_)
            | Reply::Tables(_)
            | Reply::AllColumns(_)
            | Reply::AllFks(_)
            | Reply::Objects(_)
            | Reply::Location(_)
            | Reply::Admin { .. }
            | Reply::Tx(_)
            | Reply::TxEnded { .. } => None,
        }
    }
}

/// Runs a script file. Progress is reported at most every 150 ms.
pub(crate) fn run_script(
    conn: &mut Conn,
    tx: &mut TxState,
    pg: bool,
    path: &std::path::Path,
    transaction: bool,
    cancel: &AtomicBool,
    report: impl Fn(ScriptProgress),
) -> (ScriptProgress, Option<ScriptError>) {
    use crate::script::{Item, ScriptReader};
    let mut p = ScriptProgress::default();
    let fail = |message: String, line: u64, statement: &str, cancelled: bool| ScriptError {
        message,
        line,
        statement: statement.chars().take(300).collect(),
        cancelled,
    };
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return (
                p,
                Some(fail(format!("não foi possível abrir: {e}"), 0, "", false)),
            );
        }
    };
    let kind = if pg {
        crate::db::Kind::Postgres
    } else {
        crate::db::Kind::MySql
    };
    let mut reader = ScriptReader::new(std::io::BufReader::with_capacity(1 << 20, file), kind);
    // Inside a transaction the user opened, a savepoint makes the file all-or-nothing.
    let nested = tx.open;
    let (begin, mut commit, mut rollback) = if nested {
        (
            "SAVEPOINT dblite_script",
            "RELEASE SAVEPOINT dblite_script",
            "ROLLBACK TO SAVEPOINT dblite_script",
        )
    } else if pg {
        ("BEGIN", "COMMIT", "ROLLBACK")
    } else {
        ("START TRANSACTION", "COMMIT", "ROLLBACK")
    };
    if transaction && let Err(e) = conn.query(begin) {
        return (p, Some(fail(e.to_string(), 0, begin, false)));
    }
    let mut last_report = std::time::Instant::now();
    let error = loop {
        if cancel.load(Ordering::Relaxed) {
            break Some(fail("cancelado".into(), p.line, "", true));
        }
        let Some(item) = reader.next() else { break None };
        p.bytes = reader.bytes;
        p.line = reader.line;
        let item = match item {
            Ok(i) => i,
            Err(e) => break Some(fail(format!("erro lendo o arquivo: {e}"), p.line, "", false)),
        };
        let outcome = match &item {
            Item::Skipped { .. } => {
                p.skipped += 1;
                Ok(())
            }
            Item::Copy { sql, data, line } => conn
                .copy_in(sql, data)
                .map(|n| p.copied += n)
                .map_err(|e| fail(e.to_string(), *line, sql, cancel.load(Ordering::Relaxed))),
            Item::Stmt { sql, line } => {
                let res = if transaction {
                    conn.query(sql).map(|_| ()).and_then(|()| {
                        // MySQL ends the transaction on DDL (committing it, and the user's one
                        // too): open another so the rest of the file is still undoable.
                        if pg || !sql::mysql_implicit_commit(sql) {
                            return Ok(());
                        }
                        *tx = TxState {
                            manual: tx.manual,
                            ..Default::default()
                        };
                        (commit, rollback) = ("COMMIT", "ROLLBACK");
                        conn.query("START TRANSACTION").map(|_| ())
                    })
                } else {
                    run_user_sql(conn, tx, pg, sql).map(|_| ())
                };
                res.map(|()| p.statements += 1).map_err(|e| {
                    let cancelled = cancel.load(Ordering::Relaxed);
                    fail(e.to_string(), *line, sql, cancelled)
                })
            }
        };
        if let Err(e) = outcome {
            break Some(e);
        }
        if last_report.elapsed() >= std::time::Duration::from_millis(150) {
            report(p);
            last_report = std::time::Instant::now();
        }
    };
    if transaction {
        let end = if error.is_some() { rollback } else { commit };
        if let Err(e) = conn.query(end)
            && error.is_none()
        {
            return (p, Some(fail(e.to_string(), p.line, end, false)));
        }
    }
    (p, error)
}

/// "Testar conexão": connects (through the SSH tunnel, if any) on its own thread and says
/// which server answered and how fast, without touching the open session.
pub fn test_connection(cfg: ConnConfig, ctx: eframe::egui::Context) -> Receiver<Result<String, String>> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let started = std::time::Instant::now();
        let res = (|| {
            let tunnel = match &cfg.ssh {
                Some(ssh) => {
                    Some(Tunnel::open(ssh, &cfg.host, cfg.port).map_err(|e| format!("túnel SSH: {e}"))?)
                }
                None => None,
            };
            let mut conn =
                Conn::connect(&cfg, tunnel.as_ref().map(|t| t.local_port)).map_err(|e| e.to_string())?;
            let ms = started.elapsed().as_millis();
            let version = conn
                .query("SELECT version()")
                .ok()
                .and_then(|r| r.rows.into_iter().next())
                .and_then(|r| r.into_iter().next().flatten())
                .map(|v| server_name(cfg.kind, &v))
                .unwrap_or_default();
            Ok(format!("Conectou em {ms} ms · {version}"))
        })();
        let _ = tx.send(res);
        ctx.request_repaint();
    });
    rx
}

/// "PostgreSQL 16.4 (Debian …) on x86_64…" → "PostgreSQL 16.4"; MySQL's "8.0.39" → "MySQL 8.0.39".
fn server_name(kind: crate::db::Kind, version: &str) -> String {
    match kind {
        crate::db::Kind::Postgres => version.split_whitespace().take(2).collect::<Vec<_>>().join(" "),
        crate::db::Kind::MySql => format!("MySQL {}", version.split('-').next().unwrap_or(version)),
    }
}

/// Both sides of "Comparar estrutura": the target on this connection, or on its own one
/// (through its SSH tunnel), closed when done.
fn compare_with(
    conn: &mut Conn,
    source_schema: &str,
    target: Option<&ConnConfig>,
    target_schema: &str,
) -> Result<Vec<crate::compare::TableDiff>, String> {
    let kind = conn.kind();
    let source = crate::compare::snapshot(conn, source_schema).map_err(|e| e.to_string())?;
    let dest = match target {
        None => crate::compare::snapshot(conn, target_schema),
        Some(cfg) => {
            if cfg.kind != kind {
                return Err("os dois lados precisam ser do mesmo tipo de banco".into());
            }
            let tunnel = match &cfg.ssh {
                Some(ssh) => {
                    Some(Tunnel::open(ssh, &cfg.host, cfg.port).map_err(|e| format!("túnel SSH: {e}"))?)
                }
                None => None,
            };
            let mut other = Conn::connect(cfg, tunnel.as_ref().map(|t| t.local_port))
                .map_err(|e| format!("não foi possível conectar no destino: {e}"))?;
            crate::compare::snapshot(&mut other, target_schema)
        }
    }
    .map_err(|e| e.to_string())?;
    if dest.tables.is_empty() && source.tables.is_empty() {
        return Err("nenhuma tabela nos dois lados: confira os nomes".into());
    }
    Ok(crate::compare::diff(kind, &source, &dest))
}

/// SQL the user asked for (editor, table actions, script files), keeping the manual-commit
/// state right: a write opens the transaction, BEGIN/COMMIT typed by hand are followed, MySQL's
/// DDL commits by itself, and a Postgres error leaves the transaction failed.
fn run_user_sql(conn: &mut Conn, tx: &mut TxState, pg: bool, sql: &str) -> DbResult<QueryResult> {
    let control = sql::tx_control(sql);
    let writes = control.is_none() && sql::script_risk(sql).writes;
    let res = if writes && tx.manual && !tx.open {
        conn.begin().and_then(|()| {
            tx.open = true;
            conn.query(sql)
        })
    } else {
        conn.query(sql)
    };
    let implicit_commit = !pg && res.is_ok() && sql::mysql_implicit_commit(sql);
    match control {
        // MySQL committed everything before running DDL.
        None if implicit_commit => {
            *tx = TxState {
                manual: tx.manual,
                ..Default::default()
            }
        }
        // The user ended or started it by hand.
        Some(c) if res.is_ok() => {
            *tx = TxState {
                manual: tx.manual,
                open: c == sql::TxControl::Begin,
                ..Default::default()
            }
        }
        _ if tx.open => {
            if res.is_ok() {
                if writes {
                    tx.writes += sql::statement_ranges(sql, false).len() as u32;
                }
            } else if pg {
                tx.failed = true;
            }
        }
        _ => {}
    }
    res
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
        send(Reply::Location(conn.location()));
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
                Job::Query { tab, sql } => Reply::Rows {
                    tab,
                    res: Box::new(run_user_sql(&mut conn, &mut tx, pg, &sql)),
                    notice: None,
                },
                Job::Preview { tab, sql } => Reply::Preview {
                    tab,
                    res: conn.preview(&sql, tx.open),
                },
                Job::Compare {
                    tab,
                    source_schema,
                    target,
                    target_schema,
                } => Reply::Compared {
                    tab,
                    res: compare_with(&mut conn, &source_schema, target.as_deref(), &target_schema),
                },
                Job::Dump { tab, spec, cancel } => {
                    let last = std::cell::Cell::new(std::time::Instant::now());
                    let res = crate::dump::dump(&mut conn, &spec, &cancel, |progress| {
                        // A few times a second is plenty for a progress bar.
                        if last.get().elapsed() >= std::time::Duration::from_millis(150) {
                            last.set(std::time::Instant::now());
                            send(Reply::DumpProgress { tab, progress });
                        }
                    });
                    let cancelled = cancel.load(Ordering::Relaxed);
                    match res {
                        Ok(progress) => Reply::DumpDone {
                            tab,
                            progress,
                            res: Ok(spec.path),
                        },
                        Err(e) => Reply::DumpDone {
                            tab,
                            progress: Default::default(),
                            res: Err(if cancelled {
                                "cancelado".into()
                            } else {
                                e.to_string()
                            }),
                        },
                    }
                }
                Job::Distinct { tab, sql } => Reply::Distinct {
                    tab,
                    res: conn.query(&sql),
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
                Job::RunScript {
                    tab,
                    path,
                    transaction,
                    cancel,
                } => {
                    let (progress, error) =
                        run_script(&mut conn, &mut tx, pg, &path, transaction, &cancel, |p| {
                            send(Reply::ScriptProgress { tab, progress: p })
                        });
                    // Scripts usually create or change tables.
                    send(Reply::Tables(conn.tables()));
                    send(Reply::AllColumns(conn.all_columns()));
                    send(Reply::Objects(conn.objects()));
                    Reply::ScriptDone { tab, progress, error }
                }
                Job::Admin { sql, done } => {
                    let res = run_user_sql(&mut conn, &mut tx, pg, &sql).map(|_| ());
                    // The sidebar shows the change (a new, renamed or dropped table).
                    if res.is_ok() {
                        send(Reply::Tables(conn.tables()));
                        send(Reply::AllColumns(conn.all_columns()));
                    }
                    Reply::Admin { done, res }
                }
                Job::UseSchema(name) => {
                    Reply::Location(conn.use_schema(&name).and_then(|()| conn.location()))
                }
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
            // Ordered by the primary key, so chunks neither skip nor repeat rows. In key order,
            // each chunk starts after the last key read; ordered by another column (which may
            // hold NULLs), by OFFSET.
            let by_key = !pk.is_empty() && q.order.is_none_or(|(c, asc)| asc && pk.len() == 1 && c == pk[0]);
            let mut writer: Option<Writer> = None;
            let mut offset = 0;
            let mut after: Option<Vec<Option<String>>> = None;
            loop {
                let sql = if by_key {
                    crate::sql::select_after(kind, &q, EXPORT_CHUNK, after.as_deref())
                } else {
                    select_range(kind, &q, EXPORT_CHUNK, offset)
                };
                let chunk = conn.query(&sql)?;
                if by_key && let Some(last) = chunk.rows.last() {
                    let at = |p: &String| chunk.columns.iter().position(|c| c == p);
                    after = Some(pk.iter().map(|p| at(p).and_then(|i| last[i].clone())).collect());
                }
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

    /// A file with a routine body, a COPY block / DELIMITER, then a failing statement: in a
    /// transaction nothing stays; without one, what ran before the error does.
    fn script_file(kind: Kind, port: u16, db: &str) {
        let mut cfg = ConnConfig::new(kind);
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        cfg.password = "pw".into();
        cfg.database = db.into();
        let mut c = Conn::connect(&cfg, None).expect("connect");
        let pg = kind == Kind::Postgres;
        let body = if pg {
            "\\set ON_ERROR_STOP on\nCREATE TABLE dbl_script (id int PRIMARY KEY, v text);\n\
             COPY dbl_script (id, v) FROM stdin;\n1\tum\n2\tdois; ainda\n\\.\n\
             CREATE FUNCTION dbl_script_f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql;\n\
             DROP FUNCTION dbl_script_f();\n"
        } else {
            "CREATE TABLE dbl_script (id int PRIMARY KEY, v text);\n\
             INSERT INTO dbl_script VALUES (1, 'um'), (2, 'dois; ainda');\n\
             DELIMITER //\nCREATE PROCEDURE dbl_script_p() BEGIN SELECT 1; SELECT 2; END //\nDELIMITER ;\n\
             DROP PROCEDURE dbl_script_p;\n"
        };
        let path = std::env::temp_dir().join(format!("dblite-script-{kind:?}-{}.sql", std::process::id()));
        let count = |c: &mut Conn| {
            c.query("SELECT count(*) FROM dbl_script")
                .ok()
                .and_then(|r| r.rows[0][0].clone())
        };
        let run = |c: &mut Conn, text: &str, transaction: bool| {
            std::fs::write(&path, text).unwrap();
            let mut tx = TxState::default();
            run_script(
                c,
                &mut tx,
                pg,
                &path,
                transaction,
                &AtomicBool::new(false),
                |_| {},
            )
        };
        c.query("DROP TABLE IF EXISTS dbl_script").unwrap();

        let (p, err) = run(&mut c, body, true);
        assert_eq!(err, None);
        assert_eq!(p.bytes, body.len() as u64);
        assert_eq!(count(&mut c).as_deref(), Some("2"));
        if pg {
            assert_eq!((p.statements, p.copied, p.skipped), (3, 2, 1));
        } else {
            assert_eq!(p.statements, 4);
        }
        c.query("DROP TABLE dbl_script").unwrap();

        // Fails on line 3 of the tail: rolled back as a whole (Postgres undoes the CREATE too).
        let broken = format!("{body}INSERT INTO dbl_script VALUES (3, 'tres');\nSELECT * FROM nao_existe;\n");
        let (_, err) = run(&mut c, &broken, true);
        let err = err.expect("error");
        assert!(err.statement.contains("nao_existe") && !err.cancelled, "{err:?}");
        assert_eq!(err.line, broken.lines().count() as u64);
        if pg {
            assert_eq!(count(&mut c), None, "the table went away with the rollback");
        } else {
            // MySQL commits on DDL (the CREATE PROCEDURE keeps rows 1 and 2), but the INSERT
            // after the last one is undone.
            assert_eq!(count(&mut c).as_deref(), Some("2"));
            c.query("DROP TABLE dbl_script").unwrap();
        }

        // Without a transaction what ran before the error stays.
        let (p, err) = run(&mut c, &broken, false);
        assert!(err.is_some());
        assert_eq!(count(&mut c).as_deref(), Some("3"));
        assert!(p.statements >= 3);
        c.query("DROP TABLE dbl_script").unwrap();

        // Cancelled before it starts: nothing runs.
        std::fs::write(&path, body).unwrap();
        let (p, err) = run_script(
            &mut c,
            &mut TxState::default(),
            pg,
            &path,
            true,
            &AtomicBool::new(true),
            |_| {},
        );
        assert!(err.is_some_and(|e| e.cancelled) && p.statements == 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    #[ignore]
    fn connection_test_says_who_answered() {
        let mut cfg = ConnConfig::new(Kind::Postgres);
        cfg.host = "127.0.0.1".into();
        cfg.port = 55432;
        cfg.password = "pw".into();
        cfg.database = "postgres".into();
        let ctx = eframe::egui::Context::default();
        let ok = test_connection(cfg.clone(), ctx.clone()).recv().unwrap().unwrap();
        assert!(
            ok.starts_with("Conectou em ") && ok.contains("PostgreSQL 1"),
            "{ok}"
        );
        cfg.password = "errada".into();
        let err = test_connection(cfg.clone(), ctx.clone())
            .recv()
            .unwrap()
            .unwrap_err();
        assert!(err.contains("password"), "{err}");
        cfg.kind = Kind::MySql;
        cfg.port = 53306;
        cfg.password = "pw".into();
        cfg.user = "root".into();
        cfg.database = "test".into();
        let ok = test_connection(cfg, ctx).recv().unwrap().unwrap();
        assert!(ok.contains("MySQL 8"), "{ok}");
    }

    /// Same connection (two schemas) and a separate one to the same server: same differences.
    #[test]
    #[ignore]
    fn compare_here_and_elsewhere() {
        let mut cfg = ConnConfig::new(Kind::Postgres);
        cfg.host = "127.0.0.1".into();
        cfg.port = 55432;
        cfg.password = "pw".into();
        cfg.database = "postgres".into();
        let mut c = Conn::connect(&cfg, None).expect("connect");
        for sql in [
            "DROP SCHEMA IF EXISTS cw_a CASCADE",
            "DROP SCHEMA IF EXISTS cw_b CASCADE",
            "CREATE SCHEMA cw_a",
            "CREATE SCHEMA cw_b",
            "CREATE TABLE cw_a.t (id int PRIMARY KEY, nome text NOT NULL)",
            "CREATE TABLE cw_b.t (id int PRIMARY KEY)",
        ] {
            c.query(sql).unwrap();
        }
        let here = compare_with(&mut c, "cw_a", None, "cw_b").unwrap();
        let there = compare_with(&mut c, "cw_a", Some(&cfg), "cw_b").unwrap();
        assert_eq!(here, there);
        assert_eq!(here.len(), 1);
        assert!(here[0].items[0].sql.contains("ADD COLUMN \"nome\" text NOT NULL"));
        let mut other = cfg.clone();
        other.kind = Kind::MySql;
        assert!(compare_with(&mut c, "cw_a", Some(&other), "cw_b").is_err());
        c.query("DROP SCHEMA cw_a CASCADE").unwrap();
        c.query("DROP SCHEMA cw_b CASCADE").unwrap();
    }

    #[test]
    #[ignore]
    fn script_file_postgres() {
        script_file(Kind::Postgres, 55432, "postgres");
    }

    #[test]
    #[ignore]
    fn script_file_mysql() {
        script_file(Kind::MySql, 53306, "test");
    }

    /// A real table, by hand: `DBLITE_EXPORT_CHECK=host:port:user:password:database:schema.table`
    /// (Postgres). Exports it whole as CSV and reports the time.
    #[test]
    #[ignore]
    fn export_real_table() {
        let Ok(spec) = std::env::var("DBLITE_EXPORT_CHECK") else {
            return;
        };
        let [host, port, user, password, database, qualified] = spec.split(':').collect::<Vec<_>>()[..]
        else {
            panic!("host:port:user:password:database:schema.table")
        };
        let (schema, table) = qualified.split_once('.').unwrap();
        let cfg = ConnConfig {
            host: host.into(),
            port: port.parse().unwrap(),
            user: user.into(),
            password: password.into(),
            database: database.into(),
            ..ConnConfig::new(Kind::Postgres)
        };
        let mut c = Conn::connect(&cfg, None).expect("connect");
        let cols = c.columns(schema, table).unwrap();
        let pk = cols.iter().filter(|c| c.pk).map(|c| c.name.clone()).collect();
        let path = std::env::temp_dir().join("dblite-export-real.csv");
        let started = std::time::Instant::now();
        let (_, rows) = export(
            &mut c,
            ExportSpec {
                format: Format::Csv,
                path: path.clone(),
                table_name: Kind::Postgres.qualified(schema, table),
                cols,
                source: ExportSource::Table {
                    schema: schema.into(),
                    table: table.into(),
                    filter: String::new(),
                    order: None,
                    pk,
                },
            },
        )
        .unwrap();
        eprintln!("{rows} linhas em {:?}", started.elapsed());
        let _ = std::fs::remove_file(path);
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
