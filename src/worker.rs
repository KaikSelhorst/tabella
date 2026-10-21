use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::search::{SearchHit, SearchTarget};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use std::path::PathBuf;

use crate::app::Waker;
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
    /// A read run again keeping up to `limit` rows ("Load more").
    MoreRows {
        tab: u64,
        sql: String,
        limit: usize,
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
        /// What to say when it's done, e.g. "customers renamed to clients".
        done: String,
    },
    /// The code of a function, trigger, view…
    Source {
        tab: u64,
        object: DbObject,
    },
    /// Which index a slow query is missing (see `advisor`); `schema` holds its unqualified tables.
    SuggestIndex {
        tab: u64,
        sql: String,
        schema: String,
    },
    /// The "Server" panel.
    ServerInfo {
        tab: u64,
    },
    /// Clear the "slow queries" counters, then show them again.
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
    /// "Filter values": a column's distinct values with their counts.
    Distinct {
        tab: u64,
        sql: String,
    },
    /// The foreign-key picker's options (a search capped at 2 s).
    FkOptions {
        tab: u64,
        sql: String,
        /// The search text they are for (a newer one may already be typed).
        search: String,
    },
    /// "Compare structure": `target: None` is another schema of this connection.
    Compare {
        tab: u64,
        source_schema: String,
        target: Option<Box<ConnConfig>>,
        target_schema: String,
    },
    /// "Export .sql".
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

impl Job {
    /// The tab it runs for, if any.
    fn tab(&self) -> Option<u64> {
        match self {
            Job::Query { tab, .. }
            | Job::MoreRows { tab, .. }
            | Job::Exec { tab, .. }
            | Job::Columns { tab, .. }
            | Job::Count { tab, .. }
            | Job::Batch { tab, .. }
            | Job::Structure { tab, .. }
            | Job::Import { tab, .. }
            | Job::Explain { tab, .. }
            | Job::Activity { tab, .. }
            | Job::SearchValue { tab, .. }
            | Job::Fks { tab }
            | Job::RunScript { tab, .. }
            | Job::Source { tab, .. }
            | Job::SuggestIndex { tab, .. }
            | Job::ServerInfo { tab }
            | Job::ResetQueryStats { tab }
            | Job::Kill { tab, .. }
            | Job::Export { tab, .. }
            | Job::Preview { tab, .. }
            | Job::ColumnStats { tab, .. }
            | Job::Distinct { tab, .. }
            | Job::FkOptions { tab, .. }
            | Job::Compare { tab, .. }
            | Job::Dump { tab, .. } => Some(*tab),
            Job::Tables | Job::UseSchema(_) | Job::Admin { .. } | Job::SetManual(_) | Job::EndTx { .. } => {
                None
            }
        }
    }

    /// Reads that can run on the session's second connection, so they don't wait behind (or
    /// hold up) the editor's statements. Only while no transaction is open: inside one they
    /// must see its uncommitted changes. The others either write, depend on the session's
    /// state, or answer a tab whose replies must come in order.
    fn runs_in_background(&self) -> bool {
        matches!(
            self,
            Job::Tables
                | Job::Export { .. }
                | Job::Dump { .. }
                | Job::SearchValue { .. }
                | Job::Compare { .. }
                | Job::Fks { .. }
                | Job::ServerInfo { .. }
                | Job::Activity { .. }
                | Job::Kill { .. }
                | Job::ResetQueryStats { .. }
                | Job::Source { .. }
                | Job::SuggestIndex { .. }
        )
    }
}

impl Job {
    /// Jobs on the first connection that later reads must wait for, rather than run on the
    /// second: ones that may open a transaction (whose changes the reads must then see) and
    /// a switch to another schema (which the second connection follows once it's done). A
    /// plain read, however long, holds nothing back.
    fn holds_back_background(&self) -> bool {
        match self {
            Job::Query { sql, .. } => sql::tx_control(sql).is_some() || sql::script_risk(sql).writes,
            Job::Exec { .. }
            | Job::Batch { .. }
            | Job::Import { .. }
            | Job::Admin { .. }
            | Job::RunScript { .. }
            | Job::UseSchema(_) => true,
            _ => false,
        }
    }
}

/// What one of a session's connections is running, so that cancelling a tab stops that tab's
/// statement and not whichever one happens to be running.
#[derive(Default)]
pub struct Running {
    tab: Mutex<Option<u64>>,
    canceller: Mutex<Option<Arc<Canceller>>>,
}

impl Running {
    fn set_tab(&self, tab: Option<u64>) {
        if let Ok(mut t) = self.tab.lock() {
            *t = tab;
        }
    }

    fn set_canceller(&self, c: Canceller) {
        if let Ok(mut slot) = self.canceller.lock() {
            *slot = Some(Arc::new(c));
        }
    }

    /// Stops the statement running for `tab`, or whatever runs with `None`. Blocks (it may
    /// open a connection to ask the server): call it off the UI thread.
    pub fn cancel(&self, tab: Option<u64>) {
        let running = self.tab.lock().ok().and_then(|t| *t);
        if running.is_none() || (tab.is_some() && running != tab) {
            return;
        }
        let canceller = self.canceller.lock().ok().and_then(|c| c.clone());
        if let Some(c) = canceller {
            let _ = c.cancel();
        }
    }
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
    /// The connection dropped (it is replaced before the next job).
    pub lost: bool,
}

/// Transaction state of the connection, reported with `Reply::Tx` whenever it changes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TxState {
    pub manual: bool,
    pub open: bool,
    /// Writing statements run since the transaction opened.
    pub writes: u64,
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
        order: Vec<(String, bool)>,
        pk: Vec<String>,
    },
}

pub struct Connected {
    pub tables: Vec<TableRef>,
}

/// A session's worker: where jobs go, where replies come from, and what each of its
/// connections is running (for cancelling).
pub struct Spawned {
    pub jobs: Sender<Job>,
    pub replies: Receiver<Reply>,
    pub running: Vec<Arc<Running>>,
}

pub enum Reply {
    Connected(DbResult<Connected>),
    /// The connection dropped and a new one took its place (`rolled_back`: an open transaction
    /// went with the old one), or could not be made.
    Reconnected {
        res: DbResult<Connected>,
        rolled_back: bool,
    },
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
    IndexAdvice {
        tab: u64,
        res: DbResult<crate::advisor::Advice>,
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
        /// The slowest statements, slowest first.
        slowest: Vec<crate::db::StatementTime>,
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
    FkOptions {
        tab: u64,
        search: String,
        res: DbResult<QueryResult>,
    },
    DumpProgress {
        tab: u64,
        progress: crate::dump::DumpProgress,
    },
    Compared {
        tab: u64,
        res: DbResult<Vec<crate::compare::TableDiff>>,
    },
    DumpDone {
        tab: u64,
        progress: crate::dump::DumpProgress,
        res: DbResult<PathBuf>,
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
        /// Stopped because the connection dropped.
        lost: bool,
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
    /// The job failed because the connection is gone.
    fn connection_lost(&self) -> bool {
        match self {
            Reply::ScriptDone { error, .. } => error.as_ref().is_some_and(|e| e.lost),
            Reply::SearchDone { lost, .. } => *lost,
            _ => self.error().is_some_and(DbError::is_connection_lost),
        }
    }

    /// The database's error a reply carries, if any.
    fn error(&self) -> Option<&DbError> {
        match self {
            Reply::Connected(Err(e))
            | Reply::Reconnected { res: Err(e), .. }
            | Reply::Tables(Err(e))
            | Reply::Columns { res: Err(e), .. }
            | Reply::Count { res: Err(e), .. }
            | Reply::AllColumns(Err(e))
            | Reply::AllFks(Err(e))
            | Reply::Objects(Err(e))
            | Reply::Location(Err(e))
            | Reply::Admin { res: Err(e), .. }
            | Reply::IndexAdvice { res: Err(e), .. }
            | Reply::Source { res: Err(e), .. }
            | Reply::Structure { res: Err(e), .. }
            | Reply::Plan { res: Err(e), .. }
            | Reply::Imported { res: Err(e), .. }
            | Reply::Exported { res: Err(e), .. }
            | Reply::ColumnStats { res: Err(e), .. }
            | Reply::Distinct { res: Err(e), .. }
            | Reply::FkOptions { res: Err(e), .. }
            | Reply::Compared { res: Err(e), .. }
            | Reply::DumpDone { res: Err(e), .. }
            | Reply::Preview { res: Err(e), .. }
            | Reply::ServerInfo { res: Err(e), .. }
            | Reply::Fks { res: Err(e), .. }
            | Reply::TxEnded { res: Err(e), .. } => Some(e),
            Reply::Rows { res, .. } | Reply::Saved { res, .. } => res.as_ref().as_ref().err(),
            _ => None,
        }
    }

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
            | Reply::FkOptions { tab, .. }
            | Reply::DumpDone { tab, .. }
            | Reply::IndexAdvice { tab, .. }
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
            | Reply::Reconnected { .. }
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

/// How many of a script file's slowest statements are kept.
const SLOWEST_KEPT: usize = 10;

/// Runs a script file. Progress is reported at most every 150 ms. Each statement is timed: the
/// `SLOWEST_KEPT` slowest come back, slowest first.
pub(crate) fn run_script(
    conn: &mut Conn,
    tx: &mut TxState,
    pg: bool,
    path: &std::path::Path,
    transaction: bool,
    cancel: &AtomicBool,
    report: impl Fn(ScriptProgress),
) -> (ScriptProgress, Option<ScriptError>, Vec<crate::db::StatementTime>) {
    let mut slowest: Vec<crate::db::StatementTime> = Vec::new();
    let mut keep = |sql: &str, line: u64, elapsed: std::time::Duration, rows: u64| {
        // Cheap test first: most statements are faster than the tenth slowest.
        if slowest.len() == SLOWEST_KEPT && slowest.last().is_some_and(|s| s.elapsed >= elapsed) {
            return;
        }
        let at = slowest.partition_point(|s| s.elapsed >= elapsed);
        slowest.insert(
            at,
            crate::db::StatementTime {
                line: line as usize,
                sql: sql.trim().lines().next().unwrap_or("").chars().take(80).collect(),
                elapsed,
                rows,
                returned_rows: false,
            },
        );
        slowest.truncate(SLOWEST_KEPT);
    };
    use crate::script::{Item, ScriptReader};
    let mut p = ScriptProgress::default();
    let fail = |message: String, line: u64, statement: &str, cancelled: bool| ScriptError {
        message,
        line,
        statement: statement.chars().take(300).collect(),
        cancelled,
        lost: false,
    };
    let fail_db = |e: DbError, line: u64, statement: &str, cancelled: bool| ScriptError {
        lost: e.is_connection_lost(),
        ..fail(e.to_string(), line, statement, cancelled)
    };
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return (
                p,
                Some(fail(format!("couldn't open: {e}"), 0, "", false)),
                Vec::new(),
            );
        }
    };
    let kind = conn.kind();
    let mut reader = ScriptReader::new(std::io::BufReader::with_capacity(1 << 20, file), kind);
    // Inside a transaction the user opened, a savepoint makes the file all-or-nothing.
    let nested = tx.open;
    let (begin, mut commit, mut rollback) = if nested {
        (
            "SAVEPOINT tabella_script",
            "RELEASE SAVEPOINT tabella_script",
            "ROLLBACK TO SAVEPOINT tabella_script",
        )
    } else if kind == crate::db::Kind::MySql {
        ("START TRANSACTION", "COMMIT", "ROLLBACK")
    } else {
        ("BEGIN", "COMMIT", "ROLLBACK")
    };
    if transaction && let Err(e) = conn.query(begin) {
        return (p, Some(fail_db(e, 0, begin, false)), Vec::new());
    }
    let mut last_report = std::time::Instant::now();
    let error = loop {
        if cancel.load(Ordering::Relaxed) {
            break Some(fail("cancelled".into(), p.line, "", true));
        }
        let Some(item) = reader.next() else { break None };
        p.bytes = reader.bytes;
        p.line = reader.line;
        let item = match item {
            Ok(i) => i,
            Err(e) => break Some(fail(format!("error reading the file: {e}"), p.line, "", false)),
        };
        let outcome = match &item {
            Item::Skipped { .. } => {
                p.skipped += 1;
                Ok(())
            }
            Item::Copy { sql, data, line } => {
                let begun = Instant::now();
                conn.copy_in(sql, data)
                    .map(|n| {
                        p.copied += n;
                        keep(sql, *line, begun.elapsed(), n);
                    })
                    .map_err(|e| fail_db(e, *line, sql, cancel.load(Ordering::Relaxed)))
            }
            Item::Stmt { sql, line } => {
                let begun = Instant::now();
                let res = if transaction {
                    conn.query(sql).map(|_| ()).and_then(|()| {
                        // MySQL ends the transaction on DDL (committing it, and the user's one
                        // too): open another so the rest of the file is still undoable.
                        if kind != crate::db::Kind::MySql || !sql::mysql_implicit_commit(sql) {
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
                res.map(|()| {
                    p.statements += 1;
                    keep(sql, *line, begun.elapsed(), 0);
                })
                .map_err(|e| {
                    let cancelled = cancel.load(Ordering::Relaxed);
                    fail_db(e, *line, sql, cancelled)
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
            return (p, Some(fail_db(e, p.line, end, false)), slowest);
        }
    }
    (p, error, slowest)
}

/// A read that mustn't hold the connection: Postgres gets a 2 s statement timeout inside a
/// savepoint (or a transaction of its own), undone right after, so a timeout never breaks a
/// transaction the user has open. MySQL has the limit as a hint in the SQL.
fn capped_read(conn: &mut Conn, pg: bool, in_tx: bool, sql: &str) -> DbResult<QueryResult> {
    if !pg {
        return conn.query(sql);
    }
    let (open, close) = if in_tx {
        (
            "SAVEPOINT tabella_read",
            "ROLLBACK TO SAVEPOINT tabella_read; RELEASE SAVEPOINT tabella_read",
        )
    } else {
        ("BEGIN", "ROLLBACK")
    };
    conn.query(&format!("{open}; SET LOCAL statement_timeout = 2000"))?;
    let res = conn.query(sql);
    conn.query(close)?;
    res.map_err(|e| match e.to_string() {
        m if m.contains("statement timeout") => {
            DbError::Other("the search took over 2 s: type more of the name, or the key number".into())
        }
        _ => e,
    })
}

/// "Test connection": connects (through the SSH tunnel, if any) on its own thread and says
/// which server answered and how fast, without touching the open session.
pub fn test_connection(cfg: ConnConfig, wake: Waker) -> Receiver<Result<String, String>> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let started = std::time::Instant::now();
        let res = (|| {
            let tunnel = match &cfg.ssh {
                Some(ssh) => {
                    Some(Tunnel::open(ssh, &cfg.host, cfg.port).map_err(|e| format!("SSH tunnel: {e}"))?)
                }
                None => None,
            };
            let mut conn =
                Conn::connect(&cfg, tunnel.as_ref().map(|t| t.local_port)).map_err(|e| e.to_string())?;
            let ms = started.elapsed().as_millis();
            let version = conn
                .query(if cfg.kind == crate::db::Kind::Sqlite {
                    "SELECT sqlite_version()"
                } else {
                    "SELECT version()"
                })
                .ok()
                .and_then(|r| r.rows.into_iter().next())
                .and_then(|r| r.into_iter().next().flatten())
                .map(|v| server_name(cfg.kind, &v))
                .unwrap_or_default();
            Ok(format!("Connected in {ms} ms · {version}"))
        })();
        let _ = tx.send(res);
        wake();
    });
    rx
}

/// "PostgreSQL 16.4 (Debian …) on x86_64…" → "PostgreSQL 16.4"; MySQL's "8.0.39" → "MySQL 8.0.39".
fn server_name(kind: crate::db::Kind, version: &str) -> String {
    match kind {
        crate::db::Kind::Postgres => version.split_whitespace().take(2).collect::<Vec<_>>().join(" "),
        crate::db::Kind::MySql => format!("MySQL {}", version.split('-').next().unwrap_or(version)),
        crate::db::Kind::Sqlite => format!("SQLite {version}"),
    }
}

/// Both sides of "Compare structure": the target on this connection, or on its own one
/// (through its SSH tunnel), closed when done.
fn compare_with(
    conn: &mut Conn,
    source_schema: &str,
    target: Option<&ConnConfig>,
    target_schema: &str,
) -> DbResult<Vec<crate::compare::TableDiff>> {
    let kind = conn.kind();
    let source = crate::compare::snapshot(conn, source_schema)?;
    let dest = match target {
        None => crate::compare::snapshot(conn, target_schema),
        Some(cfg) => {
            if cfg.kind != kind {
                return Err(DbError::Other(
                    "both sides must be the same kind of database".into(),
                ));
            }
            let tunnel = match &cfg.ssh {
                Some(ssh) => Some(
                    Tunnel::open(ssh, &cfg.host, cfg.port)
                        .map_err(|e| DbError::Other(format!("SSH tunnel: {e}")))?,
                ),
                None => None,
            };
            // The target's errors are its own: not this connection's, which stays up.
            let mut other = Conn::connect(cfg, tunnel.as_ref().map(|t| t.local_port))
                .map_err(|e| DbError::Other(format!("couldn't connect to the target: {e}")))?;
            crate::compare::snapshot(&mut other, target_schema)
                .map_err(|e| DbError::Other(format!("target: {e}")))
        }
    }?;
    if dest.tables.is_empty() && source.tables.is_empty() {
        return Err(DbError::Other("no tables on either side: check the names".into()));
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
    // Only MySQL commits by itself on DDL (Postgres and SQLite keep it in the transaction).
    let implicit_commit =
        conn.kind() == crate::db::Kind::MySql && res.is_ok() && sql::mysql_implicit_commit(sql);
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
                    tx.writes = tx
                        .writes
                        .saturating_add(sql::statement_ranges(sql, false).len() as u64);
                }
            } else if pg {
                tx.failed = true;
            }
        }
        _ => {}
    }
    res
}

/// Most statements, and bytes of SQL, of an editor run that is timed statement by statement.
const TIMED_MAX_STATEMENTS: usize = 100;
const TIMED_MAX_BYTES: usize = 200_000;

/// Runs what the editor sent. A script of several statements goes one statement at a time so each
/// one is timed; the result is the same the server would have given for the whole text (every
/// result set, the last statement's outcome), plus `timings`.
fn run_editor_sql(conn: &mut Conn, tx: &mut TxState, pg: bool, sql: &str) -> DbResult<QueryResult> {
    let ranges = sql::statement_ranges(sql, false);
    // Transaction control inside the script changes what "one statement at a time" means.
    if ranges.len() < 2
        || ranges.len() > TIMED_MAX_STATEMENTS
        || sql.len() > TIMED_MAX_BYTES
        || sql::tx_control(sql).is_some()
    {
        return run_user_sql(conn, tx, pg, sql);
    }
    let at: Vec<usize> = sql
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(sql.len()))
        .collect();
    // Postgres runs a multi-statement text as one transaction: keep that.
    let wrap = pg && !tx.open && !tx.manual && sql::script_risk(sql).writes;
    let started = Instant::now();
    if wrap {
        conn.query("BEGIN")?;
    }
    let mut out = QueryResult::default();
    for &(a, b) in &ranges {
        let (a, b) = (at[a], at[b]);
        // The statement keeps its place in the text (blanks in front of it), so the position an
        // error reports still points into the editor.
        let blank: String = sql[..a]
            .chars()
            .map(|c| if c == '\n' { '\n' } else { ' ' })
            .collect();
        let text = format!("{blank}{}", &sql[a..b]);
        let begun = Instant::now();
        let res = if wrap {
            conn.query(&text)
        } else {
            run_user_sql(conn, tx, pg, &text)
        };
        let mut r = match res {
            Ok(r) => r,
            Err(e) => {
                if wrap {
                    let _ = conn.query("ROLLBACK");
                }
                return Err(e);
            }
        };
        let elapsed = begun.elapsed();
        let returned_rows = !r.columns.is_empty();
        out.timings.push(crate::db::StatementTime {
            line: sql[..a].matches('\n').count() + 1,
            sql: sql[a..b]
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .chars()
                .take(80)
                .collect(),
            elapsed,
            rows: if returned_rows {
                r.rows.len() as u64
            } else {
                r.affected.unwrap_or(0)
            },
            returned_rows,
        });
        if returned_rows {
            out.next_set();
            out.columns = std::mem::take(&mut r.columns);
            out.rows = std::mem::take(&mut r.rows);
            out.truncated = r.truncated;
            out.elapsed = elapsed;
        }
        out.affected = r.affected;
    }
    if wrap {
        conn.query("COMMIT")?;
    }
    out.elapsed = started.elapsed();
    Ok(out)
}

fn send_tx(send: &impl Fn(Reply), before: TxState, now: TxState) {
    if before != now {
        send(Reply::Tx(now));
    }
}

/// Postgres opens in the first schema of the search path, usually `public`. When the sidebar
/// lists nothing there (it's empty, or holds only what extensions installed) but tables elsewhere,
/// the connection starts in the first schema that has some instead (`public` stays in the path).
/// Returns the schema switched to.
fn start_schema(conn: &mut Conn, tables: &[TableRef]) -> Option<String> {
    if conn.kind() != crate::db::Kind::Postgres {
        return None;
    }
    let (_, _, here) = conn.location().ok()?;
    if tables.iter().any(|t| t.schema == here) {
        return None;
    }
    let first = tables.first()?.schema.clone();
    conn.use_schema(&first).ok()?;
    Some(first)
}

/// Spawns the thread that owns the session's connection. Dropping `Spawned::jobs` closes it once
/// the current statement (if any) finishes; `Running::cancel` stops that statement early.
///
/// Postgres and MySQL sessions also get a second connection on a thread of its own, opened
/// once the first is up, for the reads `Job::runs_in_background` lists: a long query in one
/// tab then doesn't hold up the sidebar, an export or the server panel. A dispatcher thread
/// picks the connection for each job; until the second is up (or if it can't connect),
/// everything runs on the first.
pub fn spawn(cfg: ConnConfig, wake: Waker) -> Spawned {
    let (job_tx, job_rx) = channel::<Job>();
    let (front_tx, front_rx) = channel::<Job>();
    let (reply_tx, reply_rx) = channel::<Reply>();
    let front = Arc::new(Running::default());
    let back = Arc::new(Running::default());
    let running = vec![front.clone(), back.clone()];
    let shared = Arc::new(Shared::default());
    {
        let shared = shared.clone();
        thread::spawn(move || dispatch(&job_rx, &front_tx, &shared));
    }
    thread::spawn(move || {
        let send = |r: Reply| {
            // Only fails once the app dropped its end: nobody is left to tell.
            let _ = reply_tx.send(r);
            wake();
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
        front.set_canceller(conn.canceller(&cfg, via));
        let tables = conn.tables();
        let location = tables.as_ref().ok().and_then(|t| start_schema(&mut conn, t));
        send(Reply::Connected(tables.map(|tables| Connected { tables })));
        if cfg.kind != crate::db::Kind::Sqlite {
            let bg = spawn_background(
                cfg.clone(),
                via,
                location.clone(),
                back,
                reply_tx.clone(),
                wake.clone(),
            );
            let _ = shared.background.set(bg);
        }
        send(Reply::AllColumns(conn.all_columns()));
        send(Reply::AllFks(conn.all_fks()));
        send(Reply::Objects(conn.objects()));
        send(Reply::Location(conn.location()));
        let mut w = Worker {
            conn,
            tx: TxState {
                manual: cfg.manual_commit,
                ..Default::default()
            },
            pg: cfg.kind == crate::db::Kind::Postgres,
            cfg: &cfg,
            via,
            location,
            last_job: Instant::now(),
            send: &send,
            running: &front,
            background: false,
        };
        while let Ok(job) = front_rx.recv() {
            let holds_back = job.holds_back_background();
            let schema = match &job {
                Job::UseSchema(name) => Some(name.clone()),
                _ => None,
            };
            w.run(job);
            // The second connection follows, before the reads waiting for this go to it.
            if let (Some(name), Some(bg)) = (schema, shared.background.get())
                && w.location.as_ref() == Some(&name)
            {
                bg.send(Job::UseSchema(name));
            }
            // Stored before the count drops: `dispatch` reads them the other way round.
            shared.tx_open.store(w.tx.open, Ordering::SeqCst);
            if holds_back {
                shared.holding_back.fetch_sub(1, Ordering::SeqCst);
            }
        }
    });
    Spawned {
        jobs: job_tx,
        replies: reply_rx,
        running,
    }
}

/// What the dispatcher and the first connection's thread share.
#[derive(Default)]
struct Shared {
    /// The second connection, once its thread is started.
    background: OnceLock<Background>,
    /// A transaction is open on the first connection.
    tx_open: AtomicBool,
    /// Jobs sent to the first connection that later reads must wait for (see
    /// `Job::holds_back_background`), not finished yet.
    holding_back: AtomicUsize,
}

/// Sends each job to the second connection when it can run there, to the first otherwise.
fn dispatch(jobs: &Receiver<Job>, front: &Sender<Job>, shared: &Shared) {
    while let Ok(job) = jobs.recv() {
        if let Some(bg) = shared.background.get().filter(|bg| bg.ready())
            && job.runs_in_background()
            // The count first: when it reads 0, the thread already stored `tx_open`.
            && shared.holding_back.load(Ordering::SeqCst) == 0
            && !shared.tx_open.load(Ordering::SeqCst)
        {
            bg.send(job);
            continue;
        }
        if job.holds_back_background() {
            shared.holding_back.fetch_add(1, Ordering::SeqCst);
        }
        if front.send(job).is_err() {
            return;
        }
    }
}

/// The session's second connection, as the first thread sees it.
struct Background {
    jobs: Sender<Job>,
    /// Connected, and not lost since.
    ready: Arc<AtomicBool>,
}

impl Background {
    fn ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

/// Opens the second connection on its own thread and runs what the first thread hands it. It
/// ends when the first thread does (its sender is dropped).
fn spawn_background(
    cfg: ConnConfig,
    via: Option<u16>,
    location: Option<String>,
    running: Arc<Running>,
    reply_tx: Sender<Reply>,
    wake: Waker,
) -> Background {
    let (jobs, job_rx) = channel::<Job>();
    let ready = Arc::new(AtomicBool::new(false));
    let ready_flag = ready.clone();
    thread::spawn(move || {
        let send = |r: Reply| {
            let _ = reply_tx.send(r);
            wake();
        };
        let Ok(mut conn) = Conn::connect(&cfg, via) else {
            // Everything stays on the first connection.
            return;
        };
        if let Some(name) = &location {
            let _ = conn.use_schema(name);
        }
        running.set_canceller(conn.canceller(&cfg, via));
        ready_flag.store(true, Ordering::Relaxed);
        let mut w = Worker {
            conn,
            tx: TxState::default(),
            pg: cfg.kind == crate::db::Kind::Postgres,
            cfg: &cfg,
            via,
            location,
            last_job: Instant::now(),
            send: &send,
            running: &running,
            background: true,
        };
        while let Ok(job) = job_rx.recv() {
            if !w.run(job) {
                ready_flag.store(false, Ordering::Relaxed);
            }
        }
    });
    Background { jobs, ready }
}

/// The connection's thread: runs jobs one at a time and keeps the transaction's bookkeeping.
struct Worker<'a, S: Fn(Reply)> {
    conn: Conn,
    tx: TxState,
    pg: bool,
    cfg: &'a ConnConfig,
    /// Local port of the SSH tunnel, if any.
    via: Option<u16>,
    /// The schema or database picked with `UseSchema`: session state a new connection lacks.
    location: Option<String>,
    last_job: Instant,
    /// For replies sent before the job's own (progress, refreshed lists).
    send: &'a S,
    /// The tab whose job runs now, and how to cancel it.
    running: &'a Running,
    /// The session's second connection: it only reads, and a lost connection is replaced
    /// without telling the app (no session state of the user's was on it).
    background: bool,
}

/// MySQL isn't asked whether it is alive on every click, only after this long without a job.
const PING_AFTER_IDLE: Duration = Duration::from_secs(30);
/// Tries to get a dropped connection back, and the wait before each.
const RECONNECT_WAITS: [Duration; 3] = [Duration::ZERO, Duration::from_secs(1), Duration::from_secs(3)];

impl<S: Fn(Reply)> Worker<'_, S> {
    /// Runs one job and sends its reply. Returns false when the connection is gone and
    /// couldn't be replaced.
    fn run(&mut self, job: Job) -> bool {
        let before = self.tx;
        let mut alive = self.ensure_alive(false);
        self.running.set_tab(job.tab());
        let reply = self.handle(job);
        self.running.set_tab(None);
        if let Some(reply) = reply {
            // A job that found the connection dead (MySQL can't tell beforehand): replace
            // it now, so the run-again the user is about to do works.
            let lost = reply.connection_lost();
            (self.send)(reply);
            if lost {
                alive = self.ensure_alive(true);
            }
        }
        self.last_job = Instant::now();
        send_tx(self.send, before, self.tx);
        alive
    }

    /// A connection that dropped since the last job (idle timeout, restart, network) is replaced
    /// before the next job runs, so it doesn't fail with "connection closed". Whatever the old
    /// session held (an open transaction) is gone; the app is told.
    fn ensure_alive(&mut self, ping: bool) -> bool {
        if self
            .conn
            .is_alive(ping || self.last_job.elapsed() >= PING_AFTER_IDLE)
        {
            return true;
        }
        let mut last_error = None;
        for wait in RECONNECT_WAITS {
            thread::sleep(wait);
            match Conn::connect(self.cfg, self.via) {
                Ok(mut conn) => {
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
    fn handle(&mut self, job: Job) -> Option<Reply> {
        let conn = &mut self.conn;
        let send = self.send;
        Some(match job {
            Job::Query { tab, sql } => Reply::Rows {
                tab,
                res: Box::new(run_editor_sql(conn, &mut self.tx, self.pg, &sql)),
                notice: None,
            },
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
            Job::Admin { sql, done } => {
                let res = run_user_sql(conn, &mut self.tx, self.pg, &sql).map(|_| ());
                // The sidebar shows the change (a new, renamed or dropped table).
                if res.is_ok() {
                    send(Reply::Tables(conn.tables()));
                    send(Reply::AllColumns(conn.all_columns()));
                }
                Reply::Admin { done, res }
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
    fn open_for_write(&mut self) -> DbResult<()> {
        if self.tx.manual && !self.tx.open {
            self.conn.begin()?;
            self.tx.open = true;
        }
        Ok(())
    }

    /// Bookkeeping after a user statement ran inside the transaction.
    fn note(&mut self, ok: bool, writes: u64) {
        if self.tx.open {
            if ok {
                self.tx.writes = self.tx.writes.saturating_add(writes);
            } else if self.pg {
                self.tx.failed = true;
            }
        }
    }

    fn end_tx(&mut self, commit: bool) -> Reply {
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
    fn exec(&mut self, tab: u64, stmt: &Stmt, refresh: &str, expect_one: bool) -> Reply {
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

    fn dump(&mut self, tab: u64, spec: &crate::dump::DumpSpec, cancel: &AtomicBool) -> Reply {
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
    fn search(
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
        }
    }
}

fn export(conn: &mut Conn, spec: ExportSpec) -> DbResult<(PathBuf, u64)> {
    let io = |e: std::io::Error| DbError::Other(format!("error writing {}: {e}", spec.path.display()));
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
                order: &order,
                pk: &pk,
                page: 0,
            };
            // Ordered by the primary key, so chunks neither skip nor repeat rows. In key order,
            // each chunk starts after the last key read; ordered by another column (which may
            // hold NULLs), by OFFSET.
            let by_key = !pk.is_empty()
                && match q.order {
                    [] => true,
                    [(c, asc)] => *asc && pk.len() == 1 && *c == pk[0],
                    _ => false,
                };
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
                    after = Some(
                        pk.iter()
                            .map(|p| at(p).and_then(|i| last.cell(i).map(str::to_owned)))
                            .collect(),
                    );
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

    #[test]
    fn any_reply_can_report_a_lost_connection() {
        let dropped = || DbError::MySql(mysql::Error::IoError(std::io::ErrorKind::ConnectionReset.into()));
        let refused = || DbError::Other("syntax error".into());
        let exported = |e: DbError| Reply::Exported { tab: 1, res: Err(e) };
        assert!(exported(dropped()).connection_lost());
        assert!(!exported(refused()).connection_lost());
        assert!(
            Reply::Compared {
                tab: 1,
                res: Err(dropped())
            }
            .connection_lost()
        );
        let script = |lost: bool| Reply::ScriptDone {
            tab: 1,
            progress: ScriptProgress::default(),
            error: Some(ScriptError {
                message: String::new(),
                line: 1,
                statement: String::new(),
                cancelled: false,
                lost,
            }),
            slowest: Vec::new(),
        };
        assert!(script(true).connection_lost() && !script(false).connection_lost());
        let search = |lost: bool| Reply::SearchDone {
            tab: 1,
            cancelled: false,
            lost,
        };
        assert!(search(true).connection_lost() && !search(false).connection_lost());
    }

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
            Kind::Sqlite => unreachable!("server test"),
        };
        c.query(fill).unwrap();
        let path = std::env::temp_dir().join(format!("tabella-export-{kind:?}-{}.csv", std::process::id()));
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
                order: Vec::new(),
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
        assert_eq!(r.rows.cell(0, 0), Some("it's"));
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
            other
                .query("SELECT v FROM dbl_tx WHERE id = 1")
                .unwrap()
                .rows
                .cell(0, 0)
                .unwrap()
                .to_owned()
        };

        cfg.manual_commit = true;
        let Spawned { jobs, replies, .. } = spawn(cfg, Arc::new(|| {}));
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
        assert!(wait(&saved).contains("Nothing was saved"));
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
            // After an error Postgres only accepts ROLLBACK: "Commit" says so instead of
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
            assert!(wait(&ended).contains("rolled back"));
            assert_eq!(seen(&mut other), "d");
        }
        drop(jobs);
        other.query("DROP TABLE dbl_tx").unwrap();
    }

    /// A file script keeps its ten slowest statements, slowest first, with their lines.
    #[test]
    fn a_file_script_keeps_its_slowest_statements() {
        let cfg = crate::sqlite::tests::sample_db("slowest");
        let mut conn = Conn::connect(&cfg, None).unwrap();
        let dir = std::env::temp_dir().join(format!("tabella-slowest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("script.sql");
        // Fifteen quick statements, and one that counts a few million rows on line 8.
        let mut text = String::new();
        for i in 1..=15 {
            if i == 8 {
                text.push_str(
                    "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 3000000) SELECT count(*) FROM n;\n",
                );
            }
            text.push_str(&format!("SELECT {i};\n"));
        }
        std::fs::write(&path, text).unwrap();
        let (p, err, slowest) = run_script(
            &mut conn,
            &mut TxState::default(),
            false,
            &path,
            false,
            &AtomicBool::new(false),
            |_| {},
        );
        assert!(err.is_none(), "{err:?}");
        assert_eq!(p.statements, 16);
        assert_eq!(slowest.len(), SLOWEST_KEPT);
        assert_eq!(slowest[0].line, 8, "the slow one first");
        assert!(slowest[0].sql.starts_with("WITH RECURSIVE"));
        assert!(slowest.windows(2).all(|w| w[0].elapsed >= w[1].elapsed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A script from the editor is timed statement by statement and still ends with the same
    /// result sets and error position as the whole text would.
    #[test]
    fn editor_script_is_timed_per_statement() {
        let cfg = crate::sqlite::tests::sample_db("timed");
        let mut conn = Conn::connect(&cfg, None).unwrap();
        let mut tx = TxState::default();
        let sql = "CREATE TABLE tm (n INTEGER);\nINSERT INTO tm VALUES (1), (2);\nSELECT n FROM tm;\nSELECT n * 10 AS x FROM tm;";
        let r = run_editor_sql(&mut conn, &mut tx, false, sql).unwrap();
        let lines: Vec<usize> = r.timings.iter().map(|t| t.line).collect();
        assert_eq!(lines, [1, 2, 3, 4]);
        assert_eq!(r.timings[1].rows, 2, "an INSERT reports what it affected");
        assert!(!r.timings[1].returned_rows && r.timings[2].returned_rows);
        assert_eq!(r.timings[2].sql, "SELECT n FROM tm");
        assert_eq!(r.columns, ["x"], "the last result set is on screen");
        assert_eq!(r.previous.len(), 1, "the first SELECT is kept as an earlier set");
        // A single statement is left alone.
        assert!(
            run_editor_sql(&mut conn, &mut tx, false, "SELECT 1")
                .unwrap()
                .timings
                .is_empty()
        );
        // An error still points into the editor text.
        let bad = "SELECT 1;\nSELECT * FROM missing_table;";
        let e = run_editor_sql(&mut conn, &mut tx, false, bad).unwrap_err();
        let at = e.position_in(bad).expect("a position");
        assert!(bad[at..].starts_with("missing_table"), "{}", &bad[at..]);
        // Transaction control keeps the old whole-text run.
        let r = run_editor_sql(&mut conn, &mut tx, false, "BEGIN; SELECT 1; COMMIT;").unwrap();
        assert!(r.timings.is_empty());
    }

    /// SQLite through the worker, like the app uses it: connect, catalog, location, a query,
    /// and "commit manual" undoing an UPDATE. No server needed.
    #[test]
    fn sqlite_through_the_worker() {
        use std::time::Duration;
        let mut cfg = crate::sqlite::tests::sample_db("worker");
        cfg.manual_commit = true;
        let Spawned { jobs, replies, .. } = spawn(cfg.clone(), Arc::new(|| {}));
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
        let tables = wait(&|r| match r {
            Reply::Connected(res) => Some(
                res.map(|c| c.tables.len().to_string())
                    .unwrap_or_else(|e| e.to_string()),
            ),
            _ => None,
        });
        assert_eq!(tables, "3");
        let here = wait(&|r| match r {
            Reply::Location(res) => Some(res.map(|(_, db, _)| db).unwrap_or_else(|e| e.to_string())),
            _ => None,
        });
        assert!(here.starts_with("tabella-worker"), "{here}");
        let rows = |r: Reply| match r {
            Reply::Rows { res, .. } => Some(match *res {
                Ok(r) => r
                    .rows
                    .first()
                    .and_then(|row| row.cell(0).map(str::to_owned))
                    .unwrap_or_default(),
                Err(e) => e.to_string(),
            }),
            _ => None,
        };
        jobs.send(Job::Query {
            tab: 1,
            sql: "UPDATE customer SET name = 'Z' WHERE id = 1".into(),
        })
        .unwrap();
        wait(&rows);
        let tx = wait(&|r| match r {
            Reply::Tx(t) => Some(format!("{}:{}", t.open, t.writes)),
            _ => None,
        });
        assert_eq!(tx, "true:1", "a write opens the transaction");
        jobs.send(Job::EndTx { commit: false }).unwrap();
        wait(&|r| matches!(r, Reply::TxEnded { .. }).then(String::new));
        jobs.send(Job::Query {
            tab: 1,
            sql: "SELECT name FROM customer WHERE id = 1".into(),
        })
        .unwrap();
        assert_eq!(wait(&rows), "Anna", "undone");
        drop(jobs);
        let _ = std::fs::remove_file(&cfg.database);
    }

    /// A session killed on the server is replaced: the next job runs on a new connection, the
    /// picked schema is back, and an open transaction is reported as lost.
    fn reconnects_after_the_session_is_killed(kind: Kind, port: u16, db: &str) {
        use std::time::Duration;
        let mut cfg = ConnConfig::new(kind);
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        cfg.password = "pw".into();
        cfg.database = db.into();
        let mut other = Conn::connect(&cfg, None).expect("connect");
        let Spawned { jobs, replies, .. } = spawn(cfg, Arc::new(|| {}));
        let next = || {
            replies
                .recv_timeout(Duration::from_secs(15))
                .expect("worker reply")
        };
        let rows = |sql: &str| {
            jobs.send(Job::Query {
                tab: 1,
                sql: sql.into(),
            })
            .unwrap();
            loop {
                if let Reply::Rows { res, .. } = next() {
                    return *res;
                }
            }
        };
        let id_sql = if kind == Kind::Postgres {
            "SELECT pg_backend_pid()"
        } else {
            "SELECT CONNECTION_ID()"
        };
        let id = rows(id_sql).unwrap().rows.cell(0, 0).map(str::to_owned).unwrap();
        other
            .query(&if kind == Kind::Postgres {
                format!("SELECT pg_terminate_backend({id})")
            } else {
                format!("KILL {id}")
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        // Whatever this one does, a Reconnected reply follows; the next job runs on the new session.
        let _ = rows("SELECT 1");
        let mut seen = false;
        jobs.send(Job::Query {
            tab: 1,
            sql: id_sql.into(),
        })
        .unwrap();
        loop {
            match next() {
                Reply::Reconnected { res, .. } => {
                    assert!(res.is_ok(), "{kind:?}");
                    seen = true;
                }
                Reply::Rows { res, .. } => {
                    if let Ok(r) = *res {
                        let new_id = r.rows.cell(0, 0).map(str::to_owned).unwrap();
                        assert!(seen && new_id != id, "{kind:?}: a new session after Reconnected");
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// "Suggest index" on a real server: asks for one, gets it, creates it, and is told it is covered.
    fn suggests_an_index(kind: Kind, port: u16, db: &str, schema: &str) {
        use std::time::Duration;
        let mut cfg = ConnConfig::new(kind);
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        cfg.password = "pw".into();
        cfg.database = db.into();
        let mut c = Conn::connect(&cfg, None).expect("connect");
        c.query("DROP TABLE IF EXISTS dbl_adv").unwrap();
        c.query(
            "CREATE TABLE dbl_adv (id INT PRIMARY KEY, owner INT, status VARCHAR(20), created TIMESTAMP)",
        )
        .unwrap();
        let Spawned { jobs, replies, .. } = spawn(cfg, Arc::new(|| {}));
        let ask = |sql: &str| {
            jobs.send(Job::SuggestIndex {
                tab: 1,
                sql: sql.into(),
                schema: schema.into(),
            })
            .unwrap();
            loop {
                if let Reply::IndexAdvice { res, .. } =
                    replies.recv_timeout(Duration::from_secs(15)).expect("reply")
                {
                    return res;
                }
            }
        };
        let q = "SELECT * FROM dbl_adv WHERE owner = ? AND status = ? ORDER BY created DESC LIMIT 10";
        let a = ask(q).unwrap();
        assert_eq!(a.columns, ["owner", "status", "created"], "{kind:?}");
        assert!(a.ddl.starts_with("CREATE INDEX"), "{}", a.ddl);
        c.query(&a.ddl)
            .unwrap_or_else(|e| panic!("{kind:?}: the suggestion runs: {e}\n{}", a.ddl));
        let err = ask(q).unwrap_err();
        assert!(err.to_string().contains("Already indexed"), "{kind:?}: {err}");
        c.query("DROP TABLE dbl_adv").unwrap();
    }

    /// A bulk UPDATE by expression, as the form builds it: the preview counts the right rows and
    /// running it changes only those.
    fn bulk_expression_update(kind: Kind, port: u16, db: &str) {
        let mut cfg = ConnConfig::new(kind);
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        cfg.password = "pw".into();
        cfg.database = db.into();
        let mut c = Conn::connect(&cfg, None).expect("connect");
        c.query("DROP TABLE IF EXISTS dbl_bulk").unwrap();
        c.query("CREATE TABLE dbl_bulk (id INT PRIMARY KEY, price DECIMAL(10,2), tag VARCHAR(10))")
            .unwrap();
        c.query("INSERT INTO dbl_bulk VALUES (1, 10, 'a'), (2, 20, 'b'), (3, 30, 'c')")
            .unwrap();
        let schema = if kind == Kind::Postgres { "public" } else { db };
        let pk = vec!["id".to_string()];
        let rows = vec![vec![Some("1".to_string())], vec![Some("3".to_string())]];
        let sql = crate::sql::bulk_update(
            kind,
            schema,
            "dbl_bulk",
            "price",
            "price * 1.5",
            &crate::sql::BulkScope::Keys { pk: &pk, rows: &rows },
        );
        let preview = c
            .preview(&sql, false)
            .unwrap_or_else(|e| panic!("{kind:?}: {e}\n{sql}"));
        assert_eq!(preview.count, 2, "{kind:?}: two rows would change\n{sql}");
        let prices = |c: &mut Conn| {
            c.query("SELECT CONCAT(price, '') FROM dbl_bulk ORDER BY id")
                .unwrap()
                .rows
                .into_iter()
                .map(|r| r[0].clone().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            prices(&mut c),
            ["10.00", "20.00", "30.00"],
            "the preview changed nothing"
        );
        c.query(&sql).unwrap();
        assert_eq!(prices(&mut c), ["15.00", "20.00", "45.00"], "{kind:?}");
        // By filter, an expression that reads another column's text.
        let sql = crate::sql::bulk_update(
            kind,
            schema,
            "dbl_bulk",
            "tag",
            "upper(tag)",
            &crate::sql::BulkScope::Filter("price > 20"),
        );
        assert_eq!(c.preview(&sql, false).unwrap().count, 1);
        c.query(&sql).unwrap();
        c.query("DROP TABLE dbl_bulk").unwrap();
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn bulk_expression_postgres() {
        bulk_expression_update(Kind::Postgres, 55432, "postgres");
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn bulk_expression_mysql() {
        bulk_expression_update(Kind::MySql, 53306, "test");
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn index_advice_postgres() {
        suggests_an_index(Kind::Postgres, 55432, "postgres", "public");
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn index_advice_mysql() {
        suggests_an_index(Kind::MySql, 53306, "test", "test");
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn reconnect_postgres() {
        reconnects_after_the_session_is_killed(Kind::Postgres, 55432, "postgres");
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn reconnect_mysql() {
        reconnects_after_the_session_is_killed(Kind::MySql, 53306, "test");
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn manual_commit_postgres() {
        manual_commit(Kind::Postgres, 55432, "postgres");
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn manual_commit_mysql() {
        manual_commit(Kind::MySql, 53306, "test");
    }

    /// Reads for the sidebar run on the second connection while the first is busy, but not
    /// inside an open transaction (they must see its changes); cancelling a tab stops only
    /// that tab's statement.
    fn background_connection(kind: Kind, port: u16, db: &str) {
        let mut cfg = ConnConfig::new(kind);
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        cfg.password = "pw".into();
        cfg.database = db.into();
        cfg.manual_commit = true;
        let mut setup = Conn::connect(&cfg, None).expect("connect");
        setup.query("DROP TABLE IF EXISTS dbl_bg").unwrap();
        setup.query("CREATE TABLE dbl_bg (id INT PRIMARY KEY)").unwrap();
        let sleep = |secs: u32| match kind {
            Kind::Postgres => format!("SELECT pg_sleep({secs})"),
            _ => format!("SELECT SLEEP({secs})"),
        };
        let Spawned {
            jobs,
            replies,
            running,
        } = spawn(cfg, Arc::new(|| {}));
        // Which replies came, in order: "rows:<tab>" (ok or err) and "tables".
        let next = |timeout: u64| -> Option<String> {
            let deadline = Instant::now() + Duration::from_secs(timeout);
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                match replies.recv_timeout(left).ok()? {
                    Reply::Rows { tab, res, .. } => {
                        return Some(format!("rows:{tab}:{}", if res.is_ok() { "ok" } else { "err" }));
                    }
                    Reply::Tables(_) => return Some("tables".into()),
                    _ => {}
                }
            }
            None
        };
        // The second connection opens once the first is up.
        thread::sleep(Duration::from_secs(1));

        jobs.send(Job::Query {
            tab: 1,
            sql: sleep(2),
        })
        .unwrap();
        thread::sleep(Duration::from_millis(200));
        jobs.send(Job::Tables).unwrap();
        assert_eq!(
            next(1).as_deref(),
            Some("tables"),
            "{kind:?}: the sidebar didn't wait"
        );
        assert_eq!(next(5).as_deref(), Some("rows:1:ok"));

        // A tab waiting in line isn't the one running: cancelling it stops nothing.
        jobs.send(Job::Query {
            tab: 2,
            sql: sleep(30),
        })
        .unwrap();
        thread::sleep(Duration::from_millis(300));
        running.iter().for_each(|r| r.cancel(Some(3)));
        assert_eq!(next(1), None, "{kind:?}: another tab's statement was cancelled");
        // Stopped long before its 30 s (MySQL's SLEEP then returns 1 rather than failing).
        running.iter().for_each(|r| r.cancel(Some(2)));
        let stopped = next(10).unwrap_or_default();
        assert!(stopped.starts_with("rows:2:"), "{kind:?}: its own is ({stopped})");

        // With a transaction open, the sidebar reads on the first connection, after it.
        jobs.send(Job::Query {
            tab: 1,
            sql: "INSERT INTO dbl_bg VALUES (1)".into(),
        })
        .unwrap();
        assert_eq!(next(5).as_deref(), Some("rows:1:ok"));
        jobs.send(Job::Query {
            tab: 1,
            sql: sleep(1),
        })
        .unwrap();
        jobs.send(Job::Tables).unwrap();
        assert_eq!(
            next(5).as_deref(),
            Some("rows:1:ok"),
            "{kind:?}: in order inside a transaction"
        );
        assert_eq!(next(5).as_deref(), Some("tables"));
        jobs.send(Job::EndTx { commit: false }).unwrap();
        drop(jobs);
        setup.query("DROP TABLE dbl_bg").unwrap();
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn background_connection_postgres() {
        background_connection(Kind::Postgres, 55432, "postgres");
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn background_connection_mysql() {
        background_connection(Kind::MySql, 53306, "test");
    }

    /// Timed editor scripts on a real server: Postgres keeps the all-or-nothing of a
    /// multi-statement text, and an error points at the failing statement in the text.
    fn timed_script(kind: Kind, port: u16, db: &str) {
        let mut cfg = ConnConfig::new(kind);
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        cfg.password = "pw".into();
        cfg.database = db.into();
        if kind == Kind::Postgres {
            cfg.user = "postgres".into();
        } else {
            cfg.user = "root".into();
        }
        let mut conn = Conn::connect(&cfg, None).unwrap();
        let pg = kind == Kind::Postgres;
        let mut tx = TxState::default();
        conn.query("DROP TABLE IF EXISTS dbl_timed").unwrap();
        conn.query("CREATE TABLE dbl_timed (n INT PRIMARY KEY)").unwrap();
        let sql =
            "INSERT INTO dbl_timed VALUES (1);\nSELECT n FROM dbl_timed;\nINSERT INTO dbl_timed VALUES (2)";
        let r = run_editor_sql(&mut conn, &mut tx, pg, sql).unwrap();
        assert_eq!(r.timings.len(), 3, "{kind:?}");
        assert_eq!(r.timings[1].rows, 1);
        let bad = "INSERT INTO dbl_timed VALUES (3);\nSELECT * FROM missing_tbl";
        let e = run_editor_sql(&mut conn, &mut tx, pg, bad).unwrap_err();
        let at = e
            .position_in(bad)
            .unwrap_or_else(|| panic!("{kind:?}: no position in {e}"));
        assert!(bad[at..].contains("missing_tbl"), "{kind:?}: {}", &bad[at..]);
        let n = conn.query("SELECT count(*) FROM dbl_timed WHERE n = 3").unwrap();
        if pg {
            assert_eq!(n.rows.cell(0, 0), Some("0"), "Postgres rolls the whole text back");
        }
        conn.query("DROP TABLE dbl_timed").unwrap();
    }

    /// A database with nothing in `public` opens in its first schema with tables.
    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn starts_in_a_schema_with_tables() {
        let mut cfg = ConnConfig::new(Kind::Postgres);
        cfg.host = "127.0.0.1".into();
        cfg.port = 55432;
        cfg.user = "postgres".into();
        cfg.password = "pw".into();
        cfg.database = "postgres".into();
        let mut admin = Conn::connect(&cfg, None).unwrap();
        admin.query("DROP DATABASE IF EXISTS dbl_start").unwrap();
        admin.query("CREATE DATABASE dbl_start").unwrap();
        cfg.database = "dbl_start".into();
        {
            let mut conn = Conn::connect(&cfg, None).unwrap();
            conn.query("CREATE SCHEMA zeta; CREATE TABLE zeta.z (n INT); CREATE SCHEMA beta; CREATE TABLE beta.b (n INT)")
                .unwrap();
            let tables = conn.tables().unwrap();
            assert_eq!(start_schema(&mut conn, &tables).as_deref(), Some("beta"));
            assert_eq!(conn.location().unwrap().2, "beta");
            conn.query("SELECT n FROM b")
                .expect("unqualified names find its tables");
            // With a table of its own, public is where it opens.
            conn.query("CREATE TABLE public.p (n INT)").unwrap();
            let mut conn = Conn::connect(&cfg, None).unwrap();
            let tables = conn.tables().unwrap();
            assert_eq!(start_schema(&mut conn, &tables), None);
            assert_eq!(conn.location().unwrap().2, "public");
        }
        admin.query("DROP DATABASE dbl_start").unwrap();
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn timed_script_postgres() {
        timed_script(Kind::Postgres, 55432, "postgres");
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn timed_script_mysql() {
        timed_script(Kind::MySql, 53306, "test");
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
             COPY dbl_script (id, v) FROM stdin;\n1\tone\n2\ttwo; still\n\\.\n\
             CREATE FUNCTION dbl_script_f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql;\n\
             DROP FUNCTION dbl_script_f();\n"
        } else {
            "CREATE TABLE dbl_script (id int PRIMARY KEY, v text);\n\
             INSERT INTO dbl_script VALUES (1, 'one'), (2, 'two; still');\n\
             DELIMITER //\nCREATE PROCEDURE dbl_script_p() BEGIN SELECT 1; SELECT 2; END //\nDELIMITER ;\n\
             DROP PROCEDURE dbl_script_p;\n"
        };
        let path = std::env::temp_dir().join(format!("tabella-script-{kind:?}-{}.sql", std::process::id()));
        let count = |c: &mut Conn| {
            c.query("SELECT count(*) FROM dbl_script")
                .ok()
                .and_then(|r| r.rows.cell(0, 0).map(str::to_owned))
        };
        let run = |c: &mut Conn, text: &str, transaction: bool| {
            std::fs::write(&path, text).unwrap();
            let mut tx = TxState::default();
            let (p, err, _) = run_script(
                c,
                &mut tx,
                pg,
                &path,
                transaction,
                &AtomicBool::new(false),
                |_| {},
            );
            (p, err)
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
        let broken =
            format!("{body}INSERT INTO dbl_script VALUES (3, 'three');\nSELECT * FROM does_not_exist;\n");
        let (_, err) = run(&mut c, &broken, true);
        let err = err.expect("error");
        assert!(
            err.statement.contains("does_not_exist") && !err.cancelled,
            "{err:?}"
        );
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
        let (p, err, _) = run_script(
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

    /// A search that runs too long stops at 2 s, says so, and leaves an open transaction (and
    /// the session's timeout) as they were.
    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn capped_reads_never_break_the_transaction() {
        let mut cfg = ConnConfig::new(Kind::Postgres);
        cfg.host = "127.0.0.1".into();
        cfg.port = 55432;
        cfg.password = "pw".into();
        cfg.database = "postgres".into();
        let mut c = Conn::connect(&cfg, None).expect("connect");
        let timeout = |c: &mut Conn| {
            c.query("SHOW statement_timeout")
                .unwrap()
                .rows
                .cell(0, 0)
                .map(str::to_owned)
        };
        let before = timeout(&mut c);
        assert!(capped_read(&mut c, true, false, "SELECT 1").is_ok());
        let err = capped_read(&mut c, true, false, "SELECT pg_sleep(3)")
            .unwrap_err()
            .to_string();
        assert!(err.contains("2 s"), "{err}");
        assert_eq!(timeout(&mut c), before);
        c.query("BEGIN").unwrap();
        c.query("CREATE TEMP TABLE dbl_cap (x int)").unwrap();
        assert!(capped_read(&mut c, true, true, "SELECT pg_sleep(3)").is_err());
        // Still usable, with what was done before the search.
        c.query("INSERT INTO dbl_cap VALUES (1)").unwrap();
        c.query("COMMIT").unwrap();
        assert_eq!(
            c.query("SELECT count(*) FROM dbl_cap").unwrap().rows.cell(0, 0),
            Some("1")
        );
        assert_eq!(timeout(&mut c), before);
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn connection_test_says_who_answered() {
        let mut cfg = ConnConfig::new(Kind::Postgres);
        cfg.host = "127.0.0.1".into();
        cfg.port = 55432;
        cfg.password = "pw".into();
        cfg.database = "postgres".into();
        let ctx: Waker = Arc::new(|| {});
        let ok = test_connection(cfg.clone(), ctx.clone()).recv().unwrap().unwrap();
        assert!(
            ok.starts_with("Connected in ") && ok.contains("PostgreSQL 1"),
            "{ok}"
        );
        cfg.password = "wrong".into();
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
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
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
            "CREATE TABLE cw_a.t (id int PRIMARY KEY, name text NOT NULL)",
            "CREATE TABLE cw_b.t (id int PRIMARY KEY)",
        ] {
            c.query(sql).unwrap();
        }
        let here = compare_with(&mut c, "cw_a", None, "cw_b").unwrap();
        let there = compare_with(&mut c, "cw_a", Some(&cfg), "cw_b").unwrap();
        assert_eq!(here, there);
        assert_eq!(here.len(), 1);
        assert!(here[0].items[0].sql.contains("ADD COLUMN \"name\" text NOT NULL"));
        let mut other = cfg.clone();
        other.kind = Kind::MySql;
        assert!(compare_with(&mut c, "cw_a", Some(&other), "cw_b").is_err());
        c.query("DROP SCHEMA cw_a CASCADE").unwrap();
        c.query("DROP SCHEMA cw_b CASCADE").unwrap();
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn script_file_postgres() {
        script_file(Kind::Postgres, 55432, "postgres");
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn script_file_mysql() {
        script_file(Kind::MySql, 53306, "test");
    }

    /// A real table, by hand: `TABELLA_EXPORT_CHECK=host:port:user:password:database:schema.table`
    /// (Postgres). Exports it whole as CSV and reports the time.
    #[test]
    #[ignore = "by hand against a real database: set TABELLA_EXPORT_CHECK"]
    fn export_real_table() {
        let Ok(spec) = std::env::var("TABELLA_EXPORT_CHECK") else {
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
        let path = std::env::temp_dir().join("tabella-export-real.csv");
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
                    order: Vec::new(),
                    pk,
                },
            },
        )
        .unwrap();
        eprintln!("{rows} rows in {:?}", started.elapsed());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn export_postgres() {
        export_whole_table(Kind::Postgres, 55432, "postgres");
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn export_mysql() {
        export_whole_table(Kind::MySql, 53306, "test");
    }
}
