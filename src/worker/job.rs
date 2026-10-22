//! What the app asks a session's worker to do, and which of its connections may do it.

use super::*;

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
        /// And when it fails, before the server's reason: "Couldn't rename customers".
        failed: String,
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
    pub(super) fn tab(&self) -> Option<u64> {
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
    pub(super) fn runs_in_background(&self) -> bool {
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
    pub(super) fn holds_back_background(&self) -> bool {
        match self {
            Job::Query { sql, .. } => sql::tx_control(sql).is_some() || sql::script_risk(sql).writes,
            Job::Exec { .. }
            | Job::Batch { .. }
            | Job::Import { .. }
            | Job::Admin { .. }
            | Job::RunScript { .. }
            | Job::UseSchema(_) => true,
            // Listed one by one, so a new job has to be thought about here.
            Job::MoreRows { .. }
            | Job::Columns { .. }
            | Job::Count { .. }
            | Job::Tables
            | Job::Structure { .. }
            | Job::Explain { .. }
            | Job::Activity { .. }
            | Job::SearchValue { .. }
            | Job::Fks { .. }
            | Job::Source { .. }
            | Job::SuggestIndex { .. }
            | Job::ServerInfo { .. }
            | Job::ResetQueryStats { .. }
            | Job::Kill { .. }
            | Job::Export { .. }
            | Job::Preview { .. }
            | Job::ColumnStats { .. }
            | Job::Distinct { .. }
            | Job::FkOptions { .. }
            | Job::Compare { .. }
            | Job::Dump { .. }
            | Job::SetManual(_)
            | Job::EndTx { .. } => false,
        }
    }
}

impl Job {
    /// Whether a job cancelled while it waited in line is answered as cancelled instead of run.
    /// Writes and scripts run anyway; the jobs with a cancel flag of their own stop at once.
    pub(super) fn skippable(&self) -> bool {
        match self {
            Job::Query { .. }
            | Job::MoreRows { .. }
            | Job::Activity { .. }
            | Job::Explain { .. }
            | Job::Preview { .. }
            | Job::Count { .. }
            | Job::Distinct { .. }
            | Job::ColumnStats { .. }
            | Job::Columns { .. }
            | Job::Structure { .. }
            | Job::Source { .. }
            | Job::ServerInfo { .. }
            | Job::Fks { .. }
            | Job::Export { .. }
            | Job::Compare { .. }
            | Job::SuggestIndex { .. } => true,
            Job::Exec { .. }
            | Job::Batch { .. }
            | Job::Import { .. }
            | Job::RunScript { .. }
            | Job::Admin { .. }
            | Job::Kill { .. }
            | Job::ResetQueryStats { .. }
            | Job::SearchValue { .. }
            | Job::Dump { .. }
            | Job::FkOptions { .. }
            | Job::Tables
            | Job::UseSchema(_)
            | Job::SetManual(_)
            | Job::EndTx { .. } => false,
        }
    }

    /// The reply saying the job failed with `err` without running (cancelled, or its code hit a
    /// bug): the tab waiting for it must hear back. `None` for the one job that has no reply.
    pub(super) fn failed(&self, err: impl Fn() -> DbError) -> Option<Reply> {
        Some(match self {
            &Job::Query { tab, .. }
            | &Job::MoreRows { tab, .. }
            | &Job::Activity { tab, .. }
            | &Job::Kill { tab, .. }
            | &Job::ResetQueryStats { tab } => Reply::Rows {
                tab,
                res: Box::new(Err(err())),
                notice: None,
            },
            &Job::Explain { tab, .. } => Reply::Plan { tab, res: Err(err()) },
            &Job::Preview { tab, .. } => Reply::Preview { tab, res: Err(err()) },
            &Job::Count { tab, .. } => Reply::Count { tab, res: Err(err()) },
            &Job::Distinct { tab, .. } => Reply::Distinct { tab, res: Err(err()) },
            &Job::ColumnStats { tab, .. } => Reply::ColumnStats { tab, res: Err(err()) },
            &Job::Columns { tab, .. } => Reply::Columns {
                tab,
                res: Err(err()),
                incoming: Vec::new(),
            },
            &Job::Structure { tab, .. } => Reply::Structure { tab, res: Err(err()) },
            &Job::Source { tab, .. } => Reply::Source { tab, res: Err(err()) },
            &Job::ServerInfo { tab } => Reply::ServerInfo { tab, res: Err(err()) },
            &Job::Fks { tab } => Reply::Fks { tab, res: Err(err()) },
            &Job::Export { tab, .. } => Reply::Exported { tab, res: Err(err()) },
            &Job::Compare { tab, .. } => Reply::Compared { tab, res: Err(err()) },
            &Job::SuggestIndex { tab, .. } => Reply::IndexAdvice { tab, res: Err(err()) },
            &Job::Exec { tab, .. } | &Job::Batch { tab, .. } => Reply::Saved {
                tab,
                res: Box::new(Err(err())),
            },
            &Job::Import { tab, .. } => Reply::Imported { tab, res: Err(err()) },
            &Job::FkOptions { tab, ref search, .. } => Reply::FkOptions {
                tab,
                search: search.clone(),
                res: Err(err()),
            },
            &Job::Dump { tab, .. } => Reply::DumpDone {
                tab,
                progress: Default::default(),
                res: Err(err()),
            },
            &Job::RunScript { tab, .. } => Reply::ScriptDone {
                tab,
                progress: ScriptProgress::default(),
                error: Some(ScriptError {
                    message: err().to_string(),
                    line: 0,
                    statement: String::new(),
                    cancelled: false,
                    lost: false,
                }),
                slowest: Vec::new(),
            },
            &Job::SearchValue { tab, .. } => Reply::SearchDone {
                tab,
                cancelled: false,
                lost: false,
                error: Some(err().to_string()),
            },
            Job::Admin { done, failed, .. } => Reply::Admin {
                done: done.clone(),
                failed: failed.clone(),
                res: Err(err()),
            },
            Job::Tables => Reply::Tables(Err(err())),
            Job::UseSchema(_) => Reply::Location(Err(err())),
            &Job::EndTx { commit } => Reply::TxEnded {
                commit,
                res: Err(err()),
            },
            Job::SetManual(_) => return None,
        })
    }
}
