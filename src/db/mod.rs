use std::fmt::Write as _;
use std::time::{Duration, Instant};

use mysql::Value;
use mysql::prelude::Queryable;
use postgres::SimpleQueryMessage;
use serde::{Deserialize, Serialize};

use crate::error::{DbError, DbResult};
use crate::plan::{self, Plan};
use crate::secrets::Secret;
use crate::sql::Stmt;
use crate::tls;

mod catalog;
mod rows;
mod server;
mod structure;

pub use crate::dialect::Kind;
pub use crate::sql::FkRef;
pub use rows::{Row, Rows};

/// Default for the most rows kept from one query; protects against `SELECT *` on huge tables.
pub const DEFAULT_MAX_ROWS: usize = 5000;

/// A preference read by every connection's worker thread, so process-wide (an atomic, not
/// threaded through each job); tests leave it at the default.
static MAX_ROWS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(DEFAULT_MAX_ROWS);

/// Most rows kept from one query (a preference, see `set_max_rows`).
pub fn max_rows() -> usize {
    MAX_ROWS.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_max_rows(n: usize) {
    MAX_ROWS.store(n.max(1), std::sync::atomic::Ordering::Relaxed);
}

/// `Debug` is written by hand below so the password never ends up in a log or panic message;
/// `Secret` wipes it from memory when dropped.
#[derive(Clone, Serialize, Deserialize)]
pub struct ConnConfig {
    /// Stable identity (secrets are stored under it), so renaming never orphans a password.
    /// Assigned by `config::load` / when first saved.
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Secret,
    pub database: String,
    #[serde(default)]
    pub ssl: bool,
    /// Writes open a transaction that the user confirms or undoes explicitly.
    #[serde(default)]
    pub manual_commit: bool,
    /// UPDATE/DELETE from the editor show what they would touch before running.
    #[serde(default)]
    pub preview_writes: bool,
    /// Skip certificate verification (self-signed servers).
    #[serde(default)]
    pub ssl_insecure: bool,
    /// Blocks writes: no grid edits, and the session itself is read-only on the server.
    #[serde(default)]
    pub read_only: bool,
    /// Environment color shown around the app while connected (e.g. red for production).
    #[serde(default)]
    pub color: Option<Tag>,
    /// Where the connection is listed together with others ("Production", "Client X"); empty
    /// for none.
    #[serde(default)]
    pub group: String,
    /// The editor asks before running any statement that changes data, not only the
    /// destructive ones.
    #[serde(default)]
    pub confirm_writes: bool,
    /// Statements running longer than this are stopped by the server. 0 = no limit.
    #[serde(default)]
    pub timeout_secs: u32,
    /// Reach the database through an SSH tunnel (key or agent authentication).
    #[serde(default)]
    pub ssh: Option<SshConfig>,
    /// Reads that don't need the session (sidebar, exports, the server panel) go to a second
    /// connection, so they don't wait behind a long query. Off for servers short of connections
    /// or behind a transaction-mode pooler.
    #[serde(default = "yes")]
    pub background_reads: bool,
}

fn yes() -> bool {
    true
}

/// Connection color tags, in the order the form offers them.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Tag {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Violet,
}

impl Tag {
    pub const ALL: [Tag; 6] = [
        Tag::Red,
        Tag::Orange,
        Tag::Yellow,
        Tag::Green,
        Tag::Blue,
        Tag::Violet,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tag::Red => "Red",
            Tag::Orange => "Orange",
            Tag::Yellow => "Yellow",
            Tag::Green => "Green",
            Tag::Blue => "Blue",
            Tag::Violet => "Violet",
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Private key file; empty uses the SSH agent / default keys.
    #[serde(default)]
    pub key_path: String,
}

impl Default for SshConfig {
    fn default() -> Self {
        SshConfig {
            host: String::new(),
            port: 22,
            user: String::new(),
            key_path: String::new(),
        }
    }
}

impl std::fmt::Debug for ConnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let password = if self.password.is_empty() { "" } else { "***" };
        f.debug_struct("ConnConfig")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &password)
            .field("database", &self.database)
            .field("ssl", &self.ssl)
            .field("ssl_insecure", &self.ssl_insecure)
            .field("manual_commit", &self.manual_commit)
            .field("preview_writes", &self.preview_writes)
            .field("read_only", &self.read_only)
            .field("color", &self.color)
            .field("group", &self.group)
            .field("confirm_writes", &self.confirm_writes)
            .field("timeout_secs", &self.timeout_secs)
            .field("ssh", &self.ssh)
            .field("background_reads", &self.background_reads)
            .finish()
    }
}

/// The saved connections by group, as indexes into `saved`: the ungrouped first, then each
/// group in the order its first connection appears.
pub fn by_group(saved: &[ConnConfig]) -> Vec<(&str, Vec<usize>)> {
    let mut groups: Vec<(&str, Vec<usize>)> = vec![("", Vec::new())];
    for (i, c) in saved.iter().enumerate() {
        let name = c.group.trim();
        match groups.iter_mut().find(|(g, _)| *g == name) {
            Some((_, idx)) => idx.push(i),
            None => groups.push((name, vec![i])),
        }
    }
    groups.retain(|(_, idx)| !idx.is_empty());
    groups
}

impl ConnConfig {
    /// Every setting, password included: a connection test's result applies only while this
    /// stays the same. A `Secret`, like the password inside it.
    pub fn test_key(&self) -> Secret {
        format!("{self:?}\0{}", self.password.as_str()).into()
    }

    /// Whether `other` connects the same way (server, account, database and the session
    /// settings applied at connect time). The password is left out: the saved copy may not
    /// carry it (it lives in the keyring).
    pub fn same_session_settings(&self, other: &ConnConfig) -> bool {
        self.kind == other.kind
            && self.host.trim() == other.host.trim()
            && self.port == other.port
            && self.user == other.user
            && self.database == other.database
            && self.ssl == other.ssl
            && self.ssl_insecure == other.ssl_insecure
            && self.read_only == other.read_only
            && self.timeout_secs == other.timeout_secs
            && self.ssh == other.ssh
    }

    /// Whether traffic to this host stays on the machine (no need for TLS).
    pub fn is_local(&self) -> bool {
        matches!(self.host.trim(), "localhost" | "127.0.0.1" | "::1" | "") || self.host.starts_with("127.")
    }

    pub fn new(kind: Kind) -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind,
            host: if kind == Kind::Sqlite {
                String::new()
            } else {
                "localhost".into()
            },
            port: kind.default_port(),
            user: match kind {
                Kind::Postgres => "postgres".into(),
                Kind::MySql => "root".into(),
                Kind::Sqlite => String::new(),
            },
            password: Secret::default(),
            database: String::new(),
            preview_writes: false,
            manual_commit: false,
            ssl: false,
            ssl_insecure: false,
            read_only: false,
            color: None,
            group: String::new(),
            confirm_writes: false,
            timeout_secs: 0,
            ssh: None,
            background_reads: true,
        }
    }
}

/// A single-column foreign key: `schema.table.column` points at `ref_schema.ref_table.ref_column`.
#[derive(Clone, Debug, PartialEq)]
pub struct FkEdge {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_column: String,
}

/// One column of the connection's tables, as `Conn::all_columns` lists them.
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaColumn {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub data_type: String,
    /// Part of the table's primary key.
    pub pk: bool,
}

/// Functions, procedures, triggers, views and sequences, for the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjKind {
    Function,
    Procedure,
    Trigger,
    View,
    Sequence,
}

impl ObjKind {
    pub fn plural(self) -> &'static str {
        match self {
            ObjKind::Function => "Functions",
            ObjKind::Procedure => "Procedures",
            ObjKind::Trigger => "Triggers",
            ObjKind::View => "Views",
            ObjKind::Sequence => "Sequences",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DbObject {
    pub schema: String,
    pub name: String,
    pub kind: ObjKind,
    /// Postgres functions and procedures: the argument types, which tell overloads apart.
    pub args: String,
    /// Triggers: the table they belong to.
    pub table: String,
    /// Postgres functions and procedures: their oid (exact, overloads included).
    pub id: String,
}

/// Outcome of `Conn::preview`.
#[derive(Clone, Debug)]
pub struct PreviewData {
    /// Rows the statement would change or delete.
    pub count: u64,
    /// Some of them: as they are now, or (`after`) as the statement would leave them.
    pub sample: QueryResult,
    pub after: bool,
}

#[derive(Default, Clone, Debug)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Rows,
    pub affected: Option<u64>,
    pub truncated: bool,
    pub elapsed: Duration,
    /// Result sets of earlier statements in the same script (oldest first); this one is the last.
    pub previous: Vec<QueryResult>,
    /// How long each statement of a multi-statement run took (empty for a single statement).
    pub timings: Vec<StatementTime>,
}

/// One statement of a script run from the editor, timed on its own.
#[derive(Clone, Debug, PartialEq)]
pub struct StatementTime {
    /// 1-based line where it starts.
    pub line: usize,
    /// Its first line, shortened.
    pub sql: String,
    pub elapsed: Duration,
    /// Rows it returned, or the rows it affected when it returned none.
    pub rows: u64,
    pub returned_rows: bool,
}

/// Most result sets kept from one script (each up to `max_rows()` rows).
const MAX_SETS: usize = 20;

impl QueryResult {
    /// Trims the cell buffers once every row is in (they grow by doubling).
    fn shrink_to_fit(&mut self) {
        self.rows.shrink_to_fit();
        for set in &mut self.previous {
            set.rows.shrink_to_fit();
        }
    }

    /// Called when another result set starts: files the current one under `previous`.
    pub(crate) fn next_set(&mut self) {
        if self.columns.is_empty() {
            return;
        }
        let timings = std::mem::take(&mut self.timings);
        let mut done = std::mem::take(self);
        self.timings = timings;
        self.previous = std::mem::take(&mut done.previous);
        if self.previous.len() < MAX_SETS {
            self.previous.push(done);
        }
    }
}

#[derive(Clone, Debug)]
pub struct TableRef {
    pub schema: String,
    pub name: String,
    pub is_view: bool,
    /// Planner's row estimate (cheap, may be stale). `None` for views or never-analyzed tables.
    pub estimate: Option<i64>,
    /// Lowercased name, computed once for the sidebar filter.
    pub search_key: String,
    /// Size on disk (data + indexes), in bytes. `None` for views.
    pub size: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub pk: bool,
    pub nullable: bool,
    /// Gets a value on INSERT when omitted (DEFAULT, serial/identity, auto_increment).
    pub has_default: bool,
    /// The row this column points to, for single-column foreign keys.
    pub references: Option<FkRef>,
    /// The allowed values of an ENUM column, in the type's order (empty for any other column).
    pub enum_values: Vec<String>,
}

/// Outcome of `Conn::server_info`.
#[derive(Clone, Debug, Default)]
pub struct ServerInfo {
    /// `(label, value)`: version, uptime, size, connections, cache hit rate.
    pub facts: Vec<(String, String)>,
    pub largest: QueryResult,
    /// Secondary indexes, least used first, with whether they back a foreign key.
    pub indexes: QueryResult,
}

/// What the activity monitor shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ActivityView {
    #[default]
    Sessions,
    /// Sessions waiting on another one's locks.
    Locks,
    /// Statements that took the most time overall (pg_stat_statements / performance_schema).
    Slow,
    /// Server overview (`Conn::server_info`), not a list of rows.
    Server,
}

/// A foreign key elsewhere pointing at this table: `schema.table.column` references our
/// `target` column.
#[derive(Clone, Debug, PartialEq)]
pub struct IncomingFk {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub target: String,
}

fn my_opts(cfg: &ConnConfig, via: Option<u16>) -> mysql::OptsBuilder {
    let host = if via.is_some() {
        "127.0.0.1".to_string()
    } else {
        cfg.host.clone()
    };
    let mut b = mysql::OptsBuilder::new()
        .ip_or_hostname(Some(host))
        .tcp_port(via.unwrap_or(cfg.port))
        .user(Some(cfg.user.clone()))
        .pass(Some(cfg.password.to_string()))
        .tcp_connect_timeout(Some(Duration::from_secs(10)))
        // Affected-row counts mean "rows matched", like Postgres: an UPDATE that writes the value a
        // row already had still reports 1, so "0 rows" really means the row is gone.
        .additional_capabilities(mysql::consts::CapabilityFlags::CLIENT_FOUND_ROWS);
    if !cfg.database.is_empty() {
        b = b.db_name(Some(cfg.database.clone()));
    }
    if cfg.ssl {
        let opts = mysql::SslOpts::default().with_danger_accept_invalid_certs(cfg.ssl_insecure);
        b = b.ssl_opts(Some(opts));
    }
    b
}

/// Stops a running statement: Postgres sends a cancel request, MySQL runs `KILL QUERY` from a
/// second connection.
pub enum Canceller {
    /// Token plus whether to skip certificate checks on the cancel connection.
    Pg(postgres::CancelToken, bool),
    My(Box<mysql::OptsBuilder>, u32),
    /// SQLite stops the running statement from any thread.
    Lite(rusqlite::InterruptHandle),
}

impl Canceller {
    pub fn cancel(&self) -> DbResult<()> {
        match self {
            Canceller::Pg(token, insecure) => Ok(token.cancel_query(tls::postgres_connector(*insecure))?),
            Canceller::My(opts, id) => {
                let mut c = mysql::Conn::new((**opts).clone())?;
                c.query_drop(format!("KILL QUERY {id}"))?;
                Ok(())
            }
            Canceller::Lite(handle) => {
                handle.interrupt();
                Ok(())
            }
        }
    }
}

/// What the "Structure" tab shows.
#[derive(Clone, Debug, Default)]
pub struct TableStructure {
    pub columns: Vec<StructColumn>,
    /// `(name, definition)`.
    pub indexes: Vec<(String, String)>,
    /// `(name, definition)`.
    pub foreign_keys: Vec<(String, String)>,
    /// `CREATE TABLE ...` (plus separate indexes on Postgres), ready to copy.
    pub ddl: String,
    /// For "Export .sql": `ddl` without the foreign keys, which come as `ALTER TABLE`s to run
    /// once every table has its data.
    pub ddl_without_fks: String,
    pub fk_statements: Vec<String>,
    /// Computed columns: they get no value in an INSERT.
    pub generated: Vec<String>,
    /// Postgres: `(column, sequence)` for defaults calling `nextval('…')` (serial), to create
    /// before the table; and every column a sequence numbers (serial, identity).
    pub sequences: Vec<(String, String)>,
    pub serial_columns: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StructColumn {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    /// Default expression; `None` when the column has none.
    pub default: Option<String>,
    pub pk: bool,
}

pub enum Conn {
    Pg(Box<postgres::Client>),
    My(mysql::Conn),
    Lite(Box<rusqlite::Connection>),
}

impl Conn {
    /// Connects to `cfg`, or through a local tunnel port when `via` is set (SSH).
    pub fn connect(cfg: &ConnConfig, via: Option<u16>) -> DbResult<Self> {
        Self::connect_as(cfg, via, "tabella")
    }

    /// `connect`, naming the session `name` on the server (Postgres `application_name`, MySQL's
    /// `program_name` attribute), so the session list tells a client's connections apart.
    pub fn connect_as(cfg: &ConnConfig, via: Option<u16>, name: &str) -> DbResult<Self> {
        let mut conn = Self::open(cfg, via, name)?;
        // SQLite: read-only is how the file is opened; there's no statement timeout to set.
        if cfg.kind == Kind::Sqlite {
            return Ok(conn);
        }
        if cfg.read_only {
            // Enforced by the server, so the SQL editor can't write either.
            conn.query(match cfg.kind {
                Kind::Postgres => "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
                Kind::MySql | Kind::Sqlite => "SET SESSION TRANSACTION READ ONLY",
            })?;
        }
        if cfg.timeout_secs > 0 {
            // Server-side limit: also covers COUNT(*) and exports, not just the SQL editor.
            // MySQL's max_execution_time only applies to SELECT statements.
            let ms = u64::from(cfg.timeout_secs) * 1000;
            conn.query(&match cfg.kind {
                Kind::Postgres => format!("SET statement_timeout = {ms}"),
                Kind::MySql | Kind::Sqlite => format!("SET SESSION max_execution_time = {ms}"),
            })?;
        }
        Ok(conn)
    }

    fn open(cfg: &ConnConfig, via: Option<u16>, name: &str) -> DbResult<Self> {
        match cfg.kind {
            Kind::Postgres => {
                let mut c = postgres::Config::new();
                c.host(&cfg.host)
                    .port(via.unwrap_or(cfg.port))
                    .user(&cfg.user)
                    .password(cfg.password.as_bytes())
                    .application_name(name)
                    .connect_timeout(Duration::from_secs(10));
                if !cfg.database.is_empty() {
                    c.dbname(&cfg.database);
                }
                if via.is_some() {
                    // Through the tunnel: TCP goes to localhost, TLS still checks the real name.
                    c.hostaddr(std::net::IpAddr::from([127, 0, 0, 1]));
                }
                let client = if cfg.ssl {
                    c.ssl_mode(postgres::config::SslMode::Require);
                    c.connect(tls::postgres_connector(cfg.ssl_insecure))
                } else {
                    // Like libpq's default `sslmode=prefer`: encrypt when the server offers TLS
                    // (without verifying the certificate), plain text otherwise.
                    c.ssl_mode(postgres::config::SslMode::Prefer);
                    c.connect(tls::postgres_connector(true))
                };
                Ok(Conn::Pg(Box::new(client?)))
            }
            Kind::MySql => {
                let attrs = std::collections::HashMap::from([("program_name", name)]);
                Ok(Conn::My(mysql::Conn::new(
                    my_opts(cfg, via).connect_attrs(Some(attrs)),
                )?))
            }
            Kind::Sqlite => Ok(Conn::Lite(Box::new(crate::sqlite::open(cfg)?))),
        }
    }

    /// A handle that can stop the statement this connection is running, from another thread.
    pub fn canceller(&self, cfg: &ConnConfig, via: Option<u16>) -> Canceller {
        match self {
            Conn::Pg(c) => Canceller::Pg(c.cancel_token(), cfg.ssl_insecure || !cfg.ssl),
            Conn::My(c) => Canceller::My(Box::new(my_opts(cfg, via)), c.connection_id()),
            Conn::Lite(c) => Canceller::Lite(c.get_interrupt_handle()),
        }
    }

    /// Whether the connection still works. Postgres knows without asking; MySQL is pinged
    /// (a round trip), SQLite is a local file.
    pub fn is_alive(&mut self, ping: bool) -> bool {
        match self {
            Conn::Pg(c) => !c.is_closed(),
            Conn::My(c) => !ping || c.ping().is_ok(),
            Conn::Lite(_) => true,
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Conn::Pg(_) => Kind::Postgres,
            Conn::My(_) => Kind::MySql,
            Conn::Lite(_) => Kind::Sqlite,
        }
    }

    /// Runs one or more statements; returns the last result set (capped at `max_rows()`).
    pub fn query(&mut self, sql: &str) -> DbResult<QueryResult> {
        self.query_limit(sql, max_rows())
    }

    /// Runs a generated statement (see `sql::Stmt`): bound parameters on MySQL, escaped literals
    /// on Postgres.
    pub fn execute(&mut self, stmt: &Stmt) -> DbResult<QueryResult> {
        let start = Instant::now();
        let mut out = match self {
            Conn::Pg(c) => pg_query(c, &stmt.render(Kind::Postgres), max_rows())?,
            Conn::My(c) => {
                let (sql, values) = stmt.with_placeholders();
                my_exec(c, &sql, values, max_rows())?
            }
            Conn::Lite(c) => {
                let (sql, values) = stmt.with_placeholders();
                crate::sqlite::exec(c, &sql, &values, max_rows())?
            }
        };
        out.elapsed = start.elapsed();
        out.shrink_to_fit();
        Ok(out)
    }

    /// Same as `query` with a bigger cap: "Load more" on a truncated result.
    pub fn query_up_to(&mut self, sql: &str, limit: usize) -> DbResult<QueryResult> {
        self.query_limit(sql, limit)
    }

    fn query_limit(&mut self, sql: &str, limit: usize) -> DbResult<QueryResult> {
        let start = Instant::now();
        let mut out = match self {
            Conn::Pg(c) => pg_query(c, sql, limit)?,
            Conn::My(c) => my_query(c, sql, limit)?,
            Conn::Lite(c) => crate::sqlite::query(c, sql, limit)?,
        };
        out.elapsed = start.elapsed();
        out.shrink_to_fit();
        Ok(out)
    }

    /// Exact row count for `SELECT COUNT(*) ...`.
    pub fn count(&mut self, sql: &str) -> DbResult<i64> {
        let r = self.query(sql)?;
        let [n] = cells(r.rows.into_iter().next().unwrap_or_default())?;
        n.parse()
            .map_err(|_| DbError::Other(format!("invalid count: {n}")))
    }

    /// Runs row edits as one transaction. Every statement must touch exactly one row; if one
    /// doesn't (the row changed or vanished in another session) or fails, nothing is kept.
    pub fn run_batch(&mut self, stmts: &[Stmt], nested: bool) -> DbResult<()> {
        self.atomically(nested, |c| {
            for (i, stmt) in stmts.iter().enumerate() {
                match c.execute(stmt) {
                    Ok(r) if r.affected == Some(1) => {}
                    Ok(_) => {
                        return Err(DbError::Other(format!(
                            "Change {} didn't find its row (it changed or was deleted in another session). Nothing was saved.",
                            i + 1
                        )));
                    }
                    Err(e) => return Err(DbError::Other(format!("Change {}: {e}. Nothing was saved.", i + 1))),
                }
            }
            Ok(())
        })
    }

    /// Runs `f` all or nothing. Inside a transaction the user opened (`nested`), a savepoint
    /// does that job, so a failure undoes only `f`'s work and the user's transaction goes on.
    pub fn atomically<T>(&mut self, nested: bool, f: impl FnOnce(&mut Conn) -> DbResult<T>) -> DbResult<T> {
        let (begin, commit, rollback) = match (nested, self.kind()) {
            (true, _) => (
                "SAVEPOINT tabella_batch",
                "RELEASE SAVEPOINT tabella_batch",
                "ROLLBACK TO SAVEPOINT tabella_batch",
            ),
            (false, Kind::Postgres | Kind::Sqlite) => ("BEGIN", "COMMIT", "ROLLBACK"),
            (false, Kind::MySql) => ("START TRANSACTION", "COMMIT", "ROLLBACK"),
        };
        self.query(begin)?;
        match f(self) {
            Ok(v) => {
                self.query(commit)?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.query(rollback);
                Err(e)
            }
        }
    }

    /// What an UPDATE or DELETE would touch. Simple forms are rewritten as reads and never run;
    /// otherwise Postgres runs it with `RETURNING *` and rolls back (a savepoint inside an open
    /// transaction). MySQL has no safe way for joined forms, so those are refused.
    pub fn preview(&mut self, sql: &str, nested: bool) -> DbResult<PreviewData> {
        use crate::sql::{PREVIEW_SAMPLE, preview_queries};
        if let Some(q) = preview_queries(sql) {
            let count = self.query(&q.count)?;
            let count = count
                .rows
                .first()
                .and_then(|r| r.first())
                .flatten()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            return Ok(PreviewData {
                count,
                sample: self.query_limit(&q.sample, PREVIEW_SAMPLE)?,
                after: false,
            });
        }
        if self.kind() == Kind::MySql {
            return Err(DbError::Other(
                "preview unavailable for UPDATE/DELETE with JOIN or multiple tables in MySQL".into(),
            ));
        }
        let (begin, rollback) = if nested {
            (
                "SAVEPOINT tabella_preview",
                "ROLLBACK TO SAVEPOINT tabella_preview",
            )
        } else {
            ("BEGIN", "ROLLBACK")
        };
        let body = sql.trim().trim_end_matches(';');
        let returning = if body.to_uppercase().contains("RETURNING") {
            ""
        } else {
            " RETURNING *"
        };
        self.query(begin)?;
        let res = self.query_limit(&format!("{body}{returning}"), PREVIEW_SAMPLE);
        let undone = self.query(rollback);
        let r = res?;
        undone?;
        Ok(PreviewData {
            count: r.affected.unwrap_or(r.rows.len() as u64),
            sample: r,
            after: true,
        })
    }

    /// pg_dump's `COPY … FROM stdin` with its rows. Returns how many rows went in.
    pub fn copy_in(&mut self, sql: &str, data: &[u8]) -> DbResult<u64> {
        use std::io::Write;
        match self {
            Conn::Pg(c) => {
                let mut w = c.copy_in(sql)?;
                w.write_all(data).map_err(|e| DbError::Other(e.to_string()))?;
                Ok(w.finish()?)
            }
            Conn::My(_) | Conn::Lite(_) => {
                Err(DbError::Other("COPY … FROM stdin only exists in Postgres".into()))
            }
        }
    }

    /// Opens a transaction for the "commit manual" mode.
    pub fn begin(&mut self) -> DbResult<()> {
        self.query(match self.kind() {
            Kind::Postgres | Kind::Sqlite => "BEGIN",
            Kind::MySql => "START TRANSACTION",
        })
        .map(|_| ())
    }
}

/// Takes exactly `N` cells from a row (NULL becomes an empty string).
fn cells<const N: usize>(row: Vec<Option<String>>) -> DbResult<[String; N]> {
    let len = row.len();
    row.into_iter()
        .map(Option::unwrap_or_default)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| DbError::Other(format!("expected {N} columns, got {len}")))
}

fn pg_query(c: &mut postgres::Client, sql: &str, limit: usize) -> DbResult<QueryResult> {
    // simple_query returns every value as text, so no per-type decoding is needed.
    let msgs = c.simple_query(sql)?;
    let mut out = QueryResult::default();
    for m in msgs {
        match m {
            SimpleQueryMessage::RowDescription(cols) => {
                out.next_set();
                out.columns = cols.iter().map(|c| c.name().to_string()).collect();
                out.rows.clear();
                out.truncated = false;
            }
            SimpleQueryMessage::Row(row) => {
                if out.columns.is_empty() {
                    out.columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                }
                if out.rows.len() >= limit || !out.rows.try_push((0..row.len()).map(|i| row.get(i))) {
                    out.truncated = true;
                }
            }
            SimpleQueryMessage::CommandComplete(n) => out.affected = Some(n),
            _ => {}
        }
    }
    Ok(out)
}

fn my_query(c: &mut mysql::Conn, sql: &str, limit: usize) -> DbResult<QueryResult> {
    my_collect(c.query_iter(sql)?, limit)
}

/// Prepared statement with positional parameters; values never become part of the SQL text.
fn my_exec(c: &mut mysql::Conn, sql: &str, values: Vec<String>, limit: usize) -> DbResult<QueryResult> {
    let params = if values.is_empty() {
        mysql::Params::Empty
    } else {
        mysql::Params::Positional(values.into_iter().map(|v| Value::Bytes(v.into_bytes())).collect())
    };
    my_collect(c.exec_iter(sql, params)?, limit)
}

fn my_collect<P: mysql::prelude::Protocol>(
    mut res: mysql::QueryResult<'_, '_, '_, P>,
    limit: usize,
) -> DbResult<QueryResult> {
    let mut out = QueryResult::default();
    while let Some(set) = res.iter() {
        out.affected = Some(set.affected_rows());
        let cols: Vec<String> = set
            .columns()
            .as_ref()
            .iter()
            .map(|c| c.name_str().to_string())
            .collect();
        if cols.is_empty() {
            continue;
        }
        out.next_set();
        out.columns = cols;
        out.rows.clear();
        out.truncated = false;
        for row in set {
            let row = row?;
            if out.rows.len() >= limit || !out.rows.try_push(row.unwrap().into_iter().map(my_value)) {
                out.truncated = true;
            }
        }
    }
    Ok(out)
}

fn my_value(v: Value) -> Option<String> {
    Some(match v {
        Value::NULL => return None,
        Value::Bytes(b) => String::from_utf8_lossy(&b).into_owned(),
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Date(y, mo, d, h, mi, s, us) => {
            if h == 0 && mi == 0 && s == 0 && us == 0 {
                format!("{y:04}-{mo:02}-{d:02}")
            } else if us == 0 {
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
            } else {
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}")
            }
        }
        Value::Time(neg, d, h, mi, s, us) => {
            let hours = d * 24 + h as u32;
            let sign = if neg { "-" } else { "" };
            if us == 0 {
                format!("{sign}{hours:02}:{mi:02}:{s:02}")
            } else {
                format!("{sign}{hours:02}:{mi:02}:{s:02}.{us:06}")
            }
        }
    })
}

#[cfg(test)]
mod tests;
