//! What a session's worker answers, and the state it reports along the way.

use super::*;

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
        failed: String,
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
        /// Why the search as a whole failed (each table's own errors come with its progress).
        error: Option<String>,
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
    pub(super) fn connection_lost(&self) -> bool {
        match self {
            Reply::ScriptDone { error, .. } => error.as_ref().is_some_and(|e| e.lost),
            Reply::SearchDone { lost, .. } => *lost,
            _ => self.error().is_some_and(DbError::is_connection_lost),
        }
    }

    /// The database's error a reply carries, if any.
    pub(super) fn error(&self) -> Option<&DbError> {
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
            // Listed one by one, so a new reply carrying an error has to be added above.
            Reply::Connected(Ok(_))
            | Reply::Reconnected { res: Ok(_), .. }
            | Reply::Tables(Ok(_))
            | Reply::Columns { res: Ok(_), .. }
            | Reply::Count { res: Ok(_), .. }
            | Reply::AllColumns(Ok(_))
            | Reply::AllFks(Ok(_))
            | Reply::Objects(Ok(_))
            | Reply::Location(Ok(_))
            | Reply::Admin { res: Ok(_), .. }
            | Reply::IndexAdvice { res: Ok(_), .. }
            | Reply::Source { res: Ok(_), .. }
            | Reply::Structure { res: Ok(_), .. }
            | Reply::Plan { res: Ok(_), .. }
            | Reply::Imported { res: Ok(_), .. }
            | Reply::Exported { res: Ok(_), .. }
            | Reply::ColumnStats { res: Ok(_), .. }
            | Reply::Distinct { res: Ok(_), .. }
            | Reply::FkOptions { res: Ok(_), .. }
            | Reply::Compared { res: Ok(_), .. }
            | Reply::DumpDone { res: Ok(_), .. }
            | Reply::Preview { res: Ok(_), .. }
            | Reply::ServerInfo { res: Ok(_), .. }
            | Reply::Fks { res: Ok(_), .. }
            | Reply::TxEnded { res: Ok(_), .. }
            | Reply::ScriptProgress { .. }
            | Reply::ScriptDone { .. }
            | Reply::DumpProgress { .. }
            | Reply::SearchProgress { .. }
            | Reply::SearchDone { .. }
            | Reply::Tx(_) => None,
        }
    }

    /// A reply that shows the tab something (rows, a plan, the server panel) rather than reports
    /// an action (a save, an export): one that arrives after a newer one of its kind is stale.
    pub fn is_view(&self) -> bool {
        match self {
            Reply::Rows { .. }
            | Reply::Columns { .. }
            | Reply::Count { .. }
            | Reply::IndexAdvice { .. }
            | Reply::Source { .. }
            | Reply::Structure { .. }
            | Reply::Plan { .. }
            | Reply::ColumnStats { .. }
            | Reply::Distinct { .. }
            | Reply::FkOptions { .. }
            | Reply::Compared { .. }
            | Reply::Preview { .. }
            | Reply::ServerInfo { .. }
            | Reply::Fks { .. } => true,
            Reply::Connected(_)
            | Reply::Reconnected { .. }
            | Reply::Tables(_)
            | Reply::AllColumns(_)
            | Reply::AllFks(_)
            | Reply::Objects(_)
            | Reply::Location(_)
            | Reply::Admin { .. }
            | Reply::ScriptProgress { .. }
            | Reply::ScriptDone { .. }
            | Reply::Imported { .. }
            | Reply::Saved { .. }
            | Reply::Exported { .. }
            | Reply::DumpProgress { .. }
            | Reply::DumpDone { .. }
            | Reply::SearchProgress { .. }
            | Reply::SearchDone { .. }
            | Reply::Tx(_)
            | Reply::TxEnded { .. } => false,
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
