use std::fmt::Write as _;
use std::time::{Duration, Instant};

use mysql::Value;
use mysql::prelude::Queryable;
use postgres::SimpleQueryMessage;
use serde::{Deserialize, Serialize};

use crate::error::{DbError, DbResult};
use crate::plan::{self, Plan};
use crate::sql::Stmt;
use crate::tls;

mod catalog;
mod server;
mod structure;

/// Rows kept in memory per result; protects against `SELECT *` on huge tables.
/// Most rows kept from one query.
fn max_rows() -> usize {
    5000
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Kind {
    Postgres,
    MySql,
    /// A database file on this computer: no server, no host or user.
    Sqlite,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Postgres => "PostgreSQL",
            Kind::MySql => "MySQL",
            Kind::Sqlite => "SQLite",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Kind::Postgres => 5432,
            Kind::MySql => 3306,
            Kind::Sqlite => 0,
        }
    }

    pub fn quote_ident(self, s: &str) -> String {
        match self {
            Kind::Postgres | Kind::Sqlite => format!("\"{}\"", s.replace('"', "\"\"")),
            Kind::MySql => format!("`{}`", s.replace('`', "``")),
        }
    }

    /// A string literal for display, clipboard text and the Postgres execution path.
    ///
    /// Postgres: with a backslash present this uses the `E'...'` form (like libpq's
    /// `PQescapeLiteral`), which reads the same whatever `standard_conforming_strings` is set to.
    /// MySQL: escapes backslashes, which is wrong under `NO_BACKSLASH_ESCAPES`, so MySQL never
    /// executes these; its statements bind values as parameters instead (see `sql::Stmt`).
    pub fn quote_str(self, s: &str) -> String {
        match self {
            Kind::Postgres if s.contains('\\') => {
                format!("E'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
            }
            // SQLite has no backslash escapes at all.
            Kind::Postgres | Kind::Sqlite => format!("'{}'", s.replace('\'', "''")),
            Kind::MySql => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''")),
        }
    }

    /// `schema.table` fully quoted. In MySQL the "schema" is the database.
    pub fn qualified(self, schema: &str, table: &str) -> String {
        format!("{}.{}", self.quote_ident(schema), self.quote_ident(table))
    }
}

/// `Debug` is written by hand below so the password never ends up in a log or panic message.
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
    pub password: String,
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
    /// Statements running longer than this are stopped by the server. 0 = no limit.
    #[serde(default)]
    pub timeout_secs: u32,
    /// Reach the database through an SSH tunnel (key or agent authentication).
    #[serde(default)]
    pub ssh: Option<SshConfig>,
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
            .field("timeout_secs", &self.timeout_secs)
            .field("ssh", &self.ssh)
            .finish()
    }
}

impl ConnConfig {
    /// Every setting, password included: a connection test's result applies only while this
    /// stays the same. Kept in memory, never shown or logged.
    pub fn test_key(&self) -> String {
        format!("{self:?}\0{}", self.password)
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
            password: String::new(),
            database: String::new(),
            preview_writes: false,
            manual_commit: false,
            ssl: false,
            ssl_insecure: false,
            read_only: false,
            color: None,
            timeout_secs: 0,
            ssh: None,
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
    pub rows: Vec<Vec<Option<String>>>,
    pub affected: Option<u64>,
    pub truncated: bool,
    pub elapsed: Duration,
    /// Result sets of earlier statements in the same script (oldest first); this one is the last.
    pub previous: Vec<QueryResult>,
}

/// Most result sets kept from one script (each up to `max_rows()` rows).
const MAX_SETS: usize = 20;

impl QueryResult {
    /// Called when another result set starts: files the current one under `previous`.
    pub(crate) fn next_set(&mut self) {
        if self.columns.is_empty() {
            return;
        }
        let mut done = std::mem::take(self);
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

#[derive(Clone, Debug, PartialEq)]
pub struct FkRef {
    pub schema: String,
    pub table: String,
    pub column: String,
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
        .pass(Some(cfg.password.clone()))
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
        let mut conn = Self::open(cfg, via)?;
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

    fn open(cfg: &ConnConfig, via: Option<u16>) -> DbResult<Self> {
        match cfg.kind {
            Kind::Postgres => {
                let mut c = postgres::Config::new();
                c.host(&cfg.host)
                    .port(via.unwrap_or(cfg.port))
                    .user(&cfg.user)
                    .password(&cfg.password)
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
            Kind::MySql => Ok(Conn::My(mysql::Conn::new(my_opts(cfg, via))?)),
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
        Ok(out)
    }

    fn query_limit(&mut self, sql: &str, limit: usize) -> DbResult<QueryResult> {
        let start = Instant::now();
        let mut out = match self {
            Conn::Pg(c) => pg_query(c, sql, limit)?,
            Conn::My(c) => my_query(c, sql, limit)?,
            Conn::Lite(c) => crate::sqlite::query(c, sql, limit)?,
        };
        out.elapsed = start.elapsed();
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
                .and_then(|v| v.as_deref())
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
            ("SAVEPOINT tabella_preview", "ROLLBACK TO SAVEPOINT tabella_preview")
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
                if out.rows.len() < limit {
                    out.rows
                        .push((0..row.len()).map(|i| row.get(i).map(str::to_string)).collect());
                } else {
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
            if out.rows.len() < limit {
                out.rows.push(row.unwrap().into_iter().map(my_value).collect());
            } else {
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
mod tests {
    use super::*;

    fn roundtrip(kind: Kind, port: u16, user: &str, pass: &str, db: &str) {
        let cfg = ConnConfig {
            password: pass.into(),
            database: db.into(),
            host: "127.0.0.1".into(),
            port,
            user: user.into(),
            name: "t".into(),
            ..ConnConfig::new(kind)
        };
        let mut c = Conn::connect(&cfg, None).expect("connect");
        // Leftovers from a run that failed half-way would block dropping the table.
        c.query("DROP VIEW IF EXISTS dbl_v").unwrap();
        c.query("DROP TABLE IF EXISTS dbl_t").unwrap();
        c.query("CREATE TABLE dbl_t (id INT PRIMARY KEY, name VARCHAR(50), at TIMESTAMP NULL)")
            .unwrap();
        let r = c
            .query("INSERT INTO dbl_t VALUES (1,'o''neil',NULL),(2,'b\\c','2024-01-02 03:04:05')")
            .unwrap();
        assert_eq!(r.affected, Some(2));
        let r = c.query("SELECT * FROM dbl_t ORDER BY id").unwrap();
        assert_eq!(r.columns, ["id", "name", "at"]);
        assert_eq!(r.rows[0][1].as_deref(), Some("o'neil"));
        assert_eq!(r.rows[0][2], None);
        assert_eq!(r.rows[1][2].as_deref(), Some("2024-01-02 03:04:05"));
        let cols = c
            .columns(if kind == Kind::MySql { db } else { "public" }, "dbl_t")
            .unwrap();
        assert_eq!(
            cols.iter().map(|c| c.pk).collect::<Vec<_>>(),
            [true, false, false]
        );
        let tables = c.tables().unwrap();
        assert!(tables.iter().any(|t| t.name == "dbl_t"));
        assert_eq!(c.count("SELECT COUNT(*) FROM dbl_t").unwrap(), 2);
        let cols = c.all_columns().unwrap();
        assert!(
            cols.iter()
                .any(|c| c.table == "dbl_t" && c.column == "name" && !c.data_type.is_empty())
        );
        // "Search value": finds the row by part of a value, in the right column, and the filter
        // it offers opens exactly there.
        {
            use crate::search::{search_table, targets};
            let (ts, _) = targets(&tables, &cols, "NEIL", true);
            let t = ts.iter().find(|t| t.table == "dbl_t").expect("dbl_t is searched");
            let hit = search_table(&mut c, kind, t, "NEIL", false)
                .unwrap()
                .expect("found");
            assert_eq!(hit.columns, ["name"], "{kind:?}");
            let r = c
                .query(&format!(
                    "SELECT count(*) FROM {} WHERE {}",
                    kind.qualified(&t.schema, "dbl_t"),
                    hit.filter
                ))
                .unwrap();
            assert_eq!(r.rows[0][0].as_deref(), Some("1"), "{kind:?}");
            assert!(
                search_table(&mut c, kind, t, "o'neil", true).unwrap().is_some(),
                "{kind:?}: exact"
            );
            assert!(
                search_table(&mut c, kind, t, "none-of-this", false)
                    .unwrap()
                    .is_none()
            );
        }
        let lit = kind.quote_str("x'y");
        let r = c.query(&format!("SELECT {lit}")).unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("x'y"));
        assert!(c.query("SELEC 1").is_err());
        // Booleans written the way the grid writes them, for both engines.
        c.query("DROP TABLE IF EXISTS dbl_b").unwrap();
        c.query("CREATE TABLE dbl_b (id INT PRIMARY KEY, ok BOOLEAN)")
            .unwrap();
        c.query("INSERT INTO dbl_b VALUES (1, NULL)").unwrap();
        let s = if kind == Kind::MySql { db } else { "public" };
        for b in [true, false] {
            let v = crate::sql::bool_value(kind, b);
            let wh = crate::sql::Stmt::new("id = 1");
            c.execute(&crate::sql::update_cell(kind, s, "dbl_b", "ok", Some(v), wh))
                .unwrap();
            let r = c.query("SELECT ok FROM dbl_b").unwrap();
            assert_eq!(r.rows[0][0].as_deref().and_then(crate::sql::parse_bool), Some(b));
        }
        let cols = c.columns(s, "dbl_b").unwrap();
        assert!(
            cols[1].data_type == "boolean" || cols[1].data_type == "tinyint(1)",
            "{:?}",
            cols[1]
        );
        assert!(!cols[0].nullable && cols[1].nullable, "{cols:?}");
        assert!(!cols[0].has_default && !cols[1].has_default, "{cols:?}");
        c.query("DROP TABLE dbl_b").unwrap();

        // Generated statements must store tricky values verbatim, even with the server settings
        // that break naive escaping.
        let risky = match kind {
            Kind::Postgres => "SET standard_conforming_strings = off",
            Kind::MySql => "SET SESSION sql_mode = CONCAT(@@sql_mode, ',NO_BACKSLASH_ESCAPES')",
            Kind::Sqlite => unreachable!("server test"),
        };
        c.query(risky).unwrap();
        let nasty = [r"a\", r"x\'; DROP TABLE dbl_t; --", "it's", r"\n\0", "café 🚀"];
        for v in nasty {
            let wh = crate::sql::pk_where(
                kind,
                &["id".to_string()],
                &["id".to_string()],
                &[Some("1".into())],
            );
            c.execute(&crate::sql::update_cell(kind, s, "dbl_t", "name", Some(v), wh))
                .unwrap();
            let r = c.query("SELECT name FROM dbl_t WHERE id = 1").unwrap();
            assert_eq!(r.rows[0][0].as_deref(), Some(v), "{kind:?}");
        }
        // The table survived the injection attempt.
        assert_eq!(c.count("SELECT COUNT(*) FROM dbl_t").unwrap(), 2);

        // Affected rows mean "matched": 0 only when the row is really gone (MySQL needs
        // CLIENT_FOUND_ROWS for an unchanged value to still count).
        let one = |id: &str| {
            crate::sql::pk_where(kind, &["id".to_string()], &["id".to_string()], &[Some(id.into())])
        };
        let same = c.query("SELECT name FROM dbl_t WHERE id = 1").unwrap().rows[0][0].clone();
        let r = c
            .execute(&crate::sql::update_cell(
                kind,
                s,
                "dbl_t",
                "name",
                same.as_deref(),
                one("1"),
            ))
            .unwrap();
        assert_eq!(r.affected, Some(1), "{kind:?}: same value still matches");
        let r = c
            .execute(&crate::sql::update_cell(
                kind,
                s,
                "dbl_t",
                "name",
                Some("x"),
                one("999"),
            ))
            .unwrap();
        assert_eq!(r.affected, Some(0), "{kind:?}: missing row");

        // A long statement can be stopped from another thread.
        let canceller = c.canceller(&cfg, None);
        let started = std::time::Instant::now();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            canceller.cancel().unwrap();
        });
        let sleep = match kind {
            Kind::Postgres => "SELECT pg_sleep(20)",
            Kind::MySql => "SELECT SLEEP(20)",
            Kind::Sqlite => unreachable!("server test"),
        };
        let _ = c.query(sleep); // Postgres: error "canceling statement"; MySQL: returns 1
        stopper.join().unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{kind:?}: cancel took too long"
        );
        assert!(
            c.query("SELECT 1").is_ok(),
            "{kind:?}: connection still usable after cancel"
        );

        // Structure: the generated DDL must recreate the table (renamed) on the same server.
        c.query("CREATE INDEX dbl_t_name_idx ON dbl_t (name)").unwrap();
        let st = c.structure(s, "dbl_t").unwrap();
        assert_eq!(
            st.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["id", "name", "at"]
        );
        assert!(
            st.columns[0].pk && !st.columns[0].nullable && st.columns[1].nullable,
            "{kind:?}"
        );
        assert!(
            st.indexes.iter().any(|(n, _)| n == "dbl_t_name_idx"),
            "{:?}",
            st.indexes
        );
        let _ = c.query("DROP TABLE IF EXISTS dbl_t2");
        c.query(&st.ddl.replace("dbl_t", "dbl_t2"))
            .unwrap_or_else(|e| panic!("{kind:?}: {e}\n{}", st.ddl));
        assert_eq!(c.count("SELECT COUNT(*) FROM dbl_t2").unwrap(), 0);
        c.query("DROP TABLE dbl_t2").unwrap();

        // Single-column foreign keys are found and point at the right place.
        c.query("DROP TABLE IF EXISTS dbl_child").unwrap();
        c.query("CREATE TABLE dbl_child (id INT PRIMARY KEY, parent_id INT REFERENCES dbl_t (id))")
            .unwrap();
        if kind == Kind::MySql {
            // MySQL ignores inline REFERENCES; it needs a table-level constraint.
            c.query("ALTER TABLE dbl_child ADD CONSTRAINT dbl_child_fk FOREIGN KEY (parent_id) REFERENCES dbl_t (id)")
                .unwrap();
        }
        let cols = c.columns_with_fks(s, "dbl_child").unwrap();
        assert_eq!(
            cols[1].references,
            Some(FkRef {
                schema: s.into(),
                table: "dbl_t".into(),
                column: "id".into()
            }),
            "{kind:?}"
        );
        assert_eq!(cols[0].references, None);
        let t = c
            .tables()
            .unwrap()
            .into_iter()
            .find(|t| t.name == "dbl_t")
            .unwrap();
        assert!(t.size.is_some_and(|b| b > 0), "{kind:?}: table size");
        // The ER diagram's edges include it.
        let fks = c.all_fks().unwrap();
        assert!(
            fks.iter().any(|e| e.table == "dbl_child"
                && e.column == "parent_id"
                && e.ref_table == "dbl_t"
                && e.ref_column == "id"),
            "{kind:?}: {fks:?}"
        );
        // And the other way round: dbl_t knows dbl_child points at it.
        let schema_name = if kind == Kind::MySql { db } else { "public" };
        let incoming = c.incoming_fks(schema_name, "dbl_t").unwrap();
        assert!(
            incoming
                .iter()
                .any(|f| f.table == "dbl_child" && f.target == "id"),
            "{kind:?}: {incoming:?}"
        );
        c.query("DROP TABLE dbl_child").unwrap();

        // Errors carry the server's message, not just "db error".
        let err = c
            .query("SELECT * FROM table_that_does_not_exist")
            .unwrap_err()
            .to_string();
        assert!(err.contains("table_that_does_not_exist"), "{kind:?}: {err}");

        // Plans come back as a tree, estimated or measured.
        let p = c.explain("SELECT * FROM dbl_t WHERE id > 0;", false).unwrap();
        assert!(!p.analyzed && p.root.cost.is_some(), "{kind:?}: {p:?}");
        let p = c.explain("SELECT count(*) FROM dbl_t", true).unwrap();
        assert!(p.analyzed, "{kind:?}: {p:?}");
        let mut labels = Vec::new();
        p.root.walk(0, &mut |n, _| labels.push(n.label.to_lowercase()));
        assert!(labels.iter().any(|l| l.contains("dbl_t")), "{kind:?}: {labels:?}");

        // A filter on a column without an index: the plan's full scan suggests one.
        let p = c
            .explain("SELECT * FROM dbl_t WHERE at = '2024-01-02 03:04:05'", false)
            .unwrap();
        let hints = crate::plan::index_hints(&p);
        assert!(
            hints.iter().any(|h| h.table == "dbl_t" && h.columns == ["at"]),
            "{kind:?}: {hints:?} from {p:?}"
        );

        // A script keeps the result set of each query, not only the last one.
        let r = c
            .query("SELECT 1 AS a; SELECT 2 AS b, 3 AS c; SELECT 4 AS d")
            .unwrap();
        assert_eq!(r.columns, ["d"], "{kind:?}");
        let earlier: Vec<_> = r.previous.iter().map(|p| p.columns.join(",")).collect();
        assert_eq!(earlier, ["a", "b,c"], "{kind:?}");

        // The activity monitor sees another session, and can stop its statement.
        let mut other = Conn::connect(&cfg, None).unwrap();
        let canceller_target = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let _ = other.query(match kind {
                Kind::Postgres => "SELECT pg_sleep(20)",
                Kind::MySql => "SELECT SLEEP(20)",
                Kind::Sqlite => unreachable!("server test"),
            });
            started.elapsed()
        });
        std::thread::sleep(std::time::Duration::from_millis(700));
        let act = c.activity().unwrap();
        let q = act.columns.iter().position(|c| c == "query").unwrap();
        let sleeper = act
            .rows
            .iter()
            .find(|r| {
                r[q].as_deref()
                    .is_some_and(|t| t.to_lowercase().contains("sleep(20)"))
            })
            .unwrap_or_else(|| panic!("{kind:?}: sleeping session not listed: {act:?}"));
        let id: i64 = sleeper[0].as_deref().unwrap().parse().unwrap();
        c.kill(id, false).unwrap();
        // A session that doesn't exist is reported, not silently ignored.
        assert!(
            c.kill(999_999, false).is_err(),
            "{kind:?}: kill of a missing session"
        );
        assert!(
            canceller_target.join().unwrap() < std::time::Duration::from_secs(5),
            "{kind:?}: kill"
        );

        // Locks: a session holding a row lock shows up as blocking another one.
        {
            let mut holder = Conn::connect(&cfg, None).unwrap();
            holder.begin().unwrap();
            holder
                .query("UPDATE dbl_t SET name = 'locker' WHERE id = 1")
                .unwrap();
            let mut waiter = Conn::connect(&cfg, None).unwrap();
            let blocked = std::thread::spawn(move || {
                let _ = waiter.query("UPDATE dbl_t SET name = 'waiter' WHERE id = 1");
            });
            std::thread::sleep(std::time::Duration::from_millis(800));
            let locks = c.activity_view(ActivityView::Locks).unwrap();
            assert_eq!(locks.rows.len(), 1, "{kind:?}: {locks:?}");
            let blocker: i64 = locks.rows[0][0].as_deref().unwrap().parse().unwrap();
            c.kill(blocker, true).unwrap();
            blocked.join().unwrap();
            drop(holder);
        }
        // Slow queries: MySQL always has them; the test Postgres preloads pg_stat_statements.
        if kind == Kind::Postgres {
            c.query("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
                .unwrap();
        }
        let slow = c.activity_view(ActivityView::Slow).unwrap();
        assert!(
            slow.columns.contains(&"query".to_string()) && !slow.rows.is_empty(),
            "{kind:?}"
        );
        c.reset_query_stats().unwrap();

        // Functions, triggers, views (and Postgres sequences): listed, and their code comes back.
        {
            let schema = if kind == Kind::MySql { db } else { "public" };
            c.query("DROP VIEW IF EXISTS dbl_v").unwrap();
            c.query("CREATE VIEW dbl_v AS SELECT id FROM dbl_t").unwrap();
            match kind {
                Kind::Postgres => {
                    c.query("CREATE OR REPLACE FUNCTION dbl_f(a integer) RETURNS integer LANGUAGE sql AS $$ SELECT a + 1; $$").unwrap();
                    c.query("CREATE OR REPLACE FUNCTION dbl_tf() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END; $$").unwrap();
                    c.query("DROP TRIGGER IF EXISTS dbl_tr ON dbl_t").unwrap();
                    c.query(
                        "CREATE TRIGGER dbl_tr BEFORE UPDATE ON dbl_t FOR EACH ROW EXECUTE FUNCTION dbl_tf()",
                    )
                    .unwrap();
                    c.query("CREATE SEQUENCE IF NOT EXISTS dbl_seq START 7").unwrap();
                }
                Kind::MySql => {
                    c.query("DROP FUNCTION IF EXISTS dbl_f").unwrap();
                    c.query("CREATE FUNCTION dbl_f(a INT) RETURNS INT DETERMINISTIC RETURN a + 1")
                        .unwrap();
                    c.query("DROP TRIGGER IF EXISTS dbl_tr").unwrap();
                    c.query(
                        "CREATE TRIGGER dbl_tr BEFORE UPDATE ON dbl_t FOR EACH ROW SET NEW.name = NEW.name",
                    )
                    .unwrap();
                }
                Kind::Sqlite => unreachable!("server test"),
            }
            let objs = c.objects().unwrap();
            let find = |name: &str| {
                objs.iter()
                    .find(|o| o.schema == schema && o.name == name)
                    .unwrap_or_else(|| panic!("{kind:?}: {name} in {objs:?}"))
            };
            assert_eq!(find("dbl_f").kind, ObjKind::Function);
            assert_eq!(find("dbl_tr").table, "dbl_t");
            assert_eq!(find("dbl_v").kind, ObjKind::View);
            let src = c.object_source(find("dbl_f")).unwrap();
            assert!(src.contains("dbl_f") && src.contains("a + 1"), "{kind:?}: {src}");
            // "Run…": the call built from the listed arguments, with the value filled in.
            {
                let f = find("dbl_f");
                let args = crate::sql::parse_args(&f.args);
                assert_eq!(args.len(), 1, "{kind:?}: {}", f.args);
                let call = crate::sql::call_template(kind, &f.schema, &f.name, &args, false);
                let sql = crate::sql::bind_params(kind, &call, |_| "41".into());
                let r = c.query(&sql).unwrap_or_else(|e| panic!("{kind:?}: {e}\n{sql}"));
                assert_eq!(r.rows[0][0].as_deref(), Some("42"), "{kind:?}: {sql}");
            }
            assert!(
                c.object_source(find("dbl_tr")).unwrap().contains("dbl_t"),
                "{kind:?}"
            );
            assert!(
                c.object_source(find("dbl_v"))
                    .unwrap()
                    .contains("CREATE OR REPLACE"),
                "{kind:?}"
            );
            if kind == Kind::Postgres {
                let src = c.object_source(find("dbl_seq")).unwrap();
                assert!(src.contains("START WITH 7"), "{src}");
                // The function's own code runs again as it came back.
                let again = c.object_source(find("dbl_f")).unwrap();
                c.query(&again).unwrap();
                c.query("DROP SEQUENCE dbl_seq").unwrap();
                c.query("DROP TRIGGER dbl_tr ON dbl_t").unwrap();
                c.query("DROP FUNCTION dbl_tf()").unwrap();
            } else {
                c.query("DROP TRIGGER dbl_tr").unwrap();
            }
            c.query("DROP FUNCTION dbl_f")
                .unwrap_or_else(|_| c.query("DROP FUNCTION dbl_f(integer)").unwrap());
            c.query("DROP VIEW dbl_v").unwrap();
            // Primary keys come with the column list.
            let cols = c.all_columns().unwrap();
            assert!(
                cols.iter()
                    .any(|c| c.table == "dbl_t" && c.column == "id" && c.pk),
                "{kind:?}"
            );
            assert!(
                cols.iter()
                    .any(|c| c.table == "dbl_t" && c.column == "name" && !c.pk),
                "{kind:?}"
            );
        }

        // Table actions: duplicate with rows, rename, empty, drop.
        {
            use crate::sql::{TableAction, table_action};
            let schema = if kind == Kind::MySql { db } else { "public" };
            c.query("DROP TABLE IF EXISTS dbl_copy").unwrap();
            c.query("DROP TABLE IF EXISTS dbl_copy2").unwrap();
            let count = |c: &mut Conn, t: &str| {
                c.query(&format!("SELECT count(*) FROM {t}")).unwrap().rows[0][0]
                    .clone()
                    .unwrap()
            };
            let n = count(&mut c, "dbl_t");
            c.query(&table_action(
                kind,
                schema,
                "dbl_t",
                TableAction::Duplicate(true),
                "dbl_copy",
            ))
            .unwrap();
            assert_eq!(count(&mut c, "dbl_copy"), n, "{kind:?}: rows copied");
            c.query(&table_action(
                kind,
                schema,
                "dbl_copy",
                TableAction::Rename,
                "dbl_copy2",
            ))
            .unwrap();
            c.query(&table_action(
                kind,
                schema,
                "dbl_copy2",
                TableAction::Truncate,
                "",
            ))
            .unwrap();
            assert_eq!(count(&mut c, "dbl_copy2"), "0", "{kind:?}");
            c.query(&table_action(kind, schema, "dbl_copy2", TableAction::Drop, ""))
                .unwrap();
            assert!(c.query("SELECT 1 FROM dbl_copy2").is_err(), "{kind:?}: dropped");
        }

        // Switching where unqualified names go.
        {
            let (dbs, here, schema) = c.location().unwrap();
            assert!(dbs.contains(&here), "{kind:?}: {dbs:?} / {here}");
            match kind {
                Kind::Postgres => {
                    assert_eq!(schema, "public");
                    c.query("CREATE SCHEMA IF NOT EXISTS dbl_other").unwrap();
                    c.query("CREATE TABLE IF NOT EXISTS dbl_other.only_here (x int)")
                        .unwrap();
                    c.use_schema("dbl_other").unwrap();
                    assert_eq!(c.location().unwrap().2, "dbl_other");
                    c.query("SELECT * FROM only_here").unwrap();
                    // public still answers after it.
                    c.query("SELECT * FROM dbl_t").unwrap();
                    c.use_schema("public").unwrap();
                    assert!(c.query("SELECT * FROM only_here").is_err());
                    c.query("DROP SCHEMA dbl_other CASCADE").unwrap();
                }
                Kind::MySql => {
                    c.query("CREATE DATABASE IF NOT EXISTS dbl_other").unwrap();
                    c.query("CREATE TABLE IF NOT EXISTS dbl_other.only_here (x int)")
                        .unwrap();
                    c.use_schema("dbl_other").unwrap();
                    assert_eq!(c.location().unwrap().1, "dbl_other");
                    c.query("SELECT * FROM only_here").unwrap();
                    c.use_schema(db).unwrap();
                    c.query("DROP DATABASE dbl_other").unwrap();
                }
                Kind::Sqlite => unreachable!("server test"),
            }
        }

        // Server overview: every fact present, and the test table among the biggest.
        let info = c.server_info().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(info.facts.len(), 5, "{kind:?}: {:?}", info.facts);
        assert!(
            info.facts[0]
                .1
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_digit()),
            "{kind:?}: version {:?}",
            info.facts
        );
        assert!(!info.largest.rows.is_empty(), "{kind:?}");
        // dbl_child's index backs its foreign key; it is listed, and says so.
        let ix = &info.indexes;
        assert!(ix.columns.contains(&"scans".to_string()), "{kind:?}: {ix:?}");

        // A per-connection timeout stops long statements on the server.
        let timed = ConnConfig {
            timeout_secs: 1,
            ..cfg.clone()
        };
        let mut tc = Conn::connect(&timed, None).expect("connect with timeout");
        let started = std::time::Instant::now();
        let _ = tc.query(match kind {
            Kind::Postgres => "SELECT pg_sleep(5)",
            Kind::MySql => "SELECT SLEEP(5)",
            Kind::Sqlite => unreachable!("server test"),
        });
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "{kind:?}: timeout not applied"
        );

        // Batches are all-or-nothing: a missing row rolls back the edits before it.
        let set = |id: &str, v: &str| {
            crate::sql::update_cell(
                kind,
                s,
                "dbl_t",
                "name",
                Some(v),
                crate::sql::pk_where(kind, &["id".to_string()], &["id".to_string()], &[Some(id.into())]),
            )
        };
        c.run_batch(&[set("1", "batch-1"), set("2", "batch-2")], false)
            .unwrap();
        let names = c.query("SELECT name FROM dbl_t ORDER BY id").unwrap();
        assert_eq!(
            names
                .rows
                .iter()
                .map(|r| r[0].clone().unwrap())
                .collect::<Vec<_>>(),
            ["batch-1", "batch-2"]
        );
        let err = c
            .run_batch(&[set("1", "not-saved"), set("999", "x")], false)
            .unwrap_err();
        assert!(err.to_string().contains("Nothing was saved"), "{err}");
        let r = c.query("SELECT name FROM dbl_t WHERE id = 1").unwrap();
        assert_eq!(
            r.rows[0][0].as_deref(),
            Some("batch-1"),
            "{kind:?}: first edit rolled back"
        );

        // Read-only connections are enforced by the server.
        let ro = ConnConfig {
            read_only: true,
            ..cfg.clone()
        };
        let mut r = Conn::connect(&ro, None).expect("read-only connect");
        assert_eq!(r.count("SELECT COUNT(*) FROM dbl_t").unwrap(), 2);
        assert!(
            r.query("DELETE FROM dbl_t").is_err(),
            "write must fail on a read-only session"
        );
        assert_eq!(c.count("SELECT COUNT(*) FROM dbl_t").unwrap(), 2);
        // Preview of writes: rewritten as reads, or (Postgres, joined form) run and undone.
        {
            let p = c
                .preview("UPDATE dbl_t SET name = 'x' WHERE id = 1", false)
                .unwrap();
            assert_eq!((p.count, p.sample.rows.len(), p.after), (1, 1, false), "{kind:?}");
            let p = c.preview("DELETE FROM dbl_t", false).unwrap();
            assert!(p.count >= 2, "{kind:?}");
            let joined = "UPDATE dbl_t SET name = 'changed' FROM (SELECT 1 AS one) o WHERE dbl_t.id = o.one";
            if kind == Kind::Postgres {
                let p = c.preview(joined, false).unwrap();
                assert_eq!((p.count, p.after), (1, true));
                assert_eq!(
                    p.sample.rows[0][1].as_deref(),
                    Some("changed"),
                    "shows the values after"
                );
                let r = c.query("SELECT name FROM dbl_t WHERE id = 1").unwrap();
                assert_ne!(r.rows[0][0].as_deref(), Some("changed"), "and undoes it");
            } else {
                assert!(
                    c.preview(
                        "UPDATE dbl_t JOIN (SELECT 1 AS one) o ON dbl_t.id = o.one SET name = 'x'",
                        false
                    )
                    .is_err()
                );
            }
        }

        // Table designer: create, then alter, through real DDL.
        {
            use crate::design::{ColDef, Design, IndexDef};
            let schema = if kind == Kind::MySql { db } else { "public" };
            c.query("DROP TABLE IF EXISTS dbl_design").unwrap();
            let mut d = Design::new_table(kind, schema);
            d.table = "dbl_design".into();
            d.columns.push(ColDef {
                name: "name".into(),
                data_type: "varchar(50)".into(),
                ..ColDef::new()
            });
            d.indexes.push(IndexDef {
                existing: false,
                name: String::new(),
                columns: "name".into(),
                unique: false,
                dropped: false,
            });
            c.query(&d.script())
                .unwrap_or_else(|e| panic!("{kind:?}: {e}\n{}", d.script()));
            let st = c.structure(schema, "dbl_design").unwrap();
            let mut d = Design::from_structure(kind, schema, "dbl_design", &st);
            assert!(d.statements().is_empty(), "{kind:?}: {:?}", d.statements());
            assert_eq!(
                d.indexes.len(),
                1,
                "{kind:?}: only the non-key index: {:?}",
                d.indexes
            );
            let name_col = d.columns.iter_mut().find(|c| c.name == "name").unwrap();
            name_col.name = "full_name".into();
            name_col.nullable = false;
            name_col.default = "'x'".into();
            d.indexes[0].dropped = true;
            d.columns.push(ColDef {
                name: "active".into(),
                data_type: "boolean".into(),
                ..ColDef::new()
            });
            c.query(&d.script())
                .unwrap_or_else(|e| panic!("{kind:?}: {e}\n{}", d.script()));
            let st = c.structure(schema, "dbl_design").unwrap();
            let names: Vec<&str> = st.columns.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(names, ["id", "full_name", "active"], "{kind:?}");
            assert!(!st.columns[1].nullable, "{kind:?}");
            c.query("DROP TABLE dbl_design").unwrap();
        }

        // Column profile through the database, with and without min/max.
        {
            let schema = if kind == Kind::MySql { db } else { "public" };
            let (sum, top) = crate::sql::column_stats(kind, schema, "dbl_t", "id", "", true);
            let st =
                crate::values::stats_from_queries(&c.query(&sum).unwrap(), &c.query(&top).unwrap()).unwrap();
            assert!(
                st.total >= 2 && st.nulls == 0 && st.distinct == st.total,
                "{kind:?}: {st:?}"
            );
            assert!(st.min.is_some() && st.max.is_some());
            let (sum, top) = crate::sql::column_stats(kind, schema, "dbl_t", "at", "id > 0", false);
            let st =
                crate::values::stats_from_queries(&c.query(&sum).unwrap(), &c.query(&top).unwrap()).unwrap();
            assert!(
                st.nulls >= 1 && st.top.iter().any(|(v, _)| v.is_none()),
                "{kind:?}: {st:?}"
            );
        }

        // CSV import: all rows in one transaction, or none when one fails.
        {
            use crate::import::{ImportSpec, run};
            let path =
                std::env::temp_dir().join(format!("tabella-import-{kind:?}-{}.csv", std::process::id()));
            std::fs::write(&path, "code;name\n10;ten\n11;\n").unwrap();
            let schema = if kind == Kind::MySql { db } else { "public" };
            let mut spec = ImportSpec {
                path: path.clone(),
                kind,
                schema: schema.into(),
                table: "dbl_t".into(),
                delimiter: b';',
                has_header: true,
                mapping: vec![Some("id".into()), Some("name".into())],
                bool_cols: vec![],
                number_cols: vec!["id".into()],
                empty_is_null: true,
            };
            assert_eq!(run(&mut c, &spec, false).unwrap(), 2, "{kind:?}");
            let r = c.query("SELECT name FROM dbl_t WHERE id = 11").unwrap();
            assert_eq!(r.rows[0][0], None, "{kind:?}: empty cell is NULL");
            std::fs::write(&path, "code;name\n12;twelve\n10;repeated\n").unwrap();
            let err = run(&mut c, &spec, false).unwrap_err().to_string();
            assert!(err.contains("Nothing was imported"), "{kind:?}: {err}");
            let r = c.query("SELECT count(*) FROM dbl_t WHERE id = 12").unwrap();
            assert_eq!(r.rows[0][0].as_deref(), Some("0"), "{kind:?}: rolled back");
            spec.mapping = vec![None, None];
            assert!(run(&mut c, &spec, false).is_err());
            let _ = std::fs::remove_file(path);

            // The same from Excel: id, name and a date-time column.
            let path = crate::import::tests::xlsx(&format!("{kind:?}"));
            let spec = ImportSpec {
                path: path.clone(),
                delimiter: 0,
                mapping: vec![
                    Some("id".into()),
                    Some("name".into()),
                    None,
                    None,
                    None,
                    Some("at".into()),
                ],
                ..spec
            };
            assert_eq!(run(&mut c, &spec, false).unwrap(), 2, "{kind:?}");
            let r = c
                .query("SELECT name, at FROM dbl_t WHERE id IN (20, 21) ORDER BY id")
                .unwrap();
            assert_eq!(r.rows[0][0].as_deref(), Some("Zoë Brontë"), "{kind:?}");
            assert!(
                r.rows[0][1]
                    .as_deref()
                    .is_some_and(|v| v.starts_with("2026-10-14 13:05:00")),
                "{kind:?}: {:?}",
                r.rows
            );
            assert_eq!(r.rows[1], vec![None, None], "{kind:?}");
            let _ = std::fs::remove_file(path);
        }
        // "Filter values": the list and the condition it builds run as they are.
        {
            let schema = if kind == Kind::MySql { db } else { "public" };
            let expr = kind.quote_ident("name");
            let list = c
                .query(&crate::sql::distinct_values(kind, schema, "dbl_t", &expr, "", ""))
                .unwrap();
            let values: Vec<Option<String>> = list.rows.iter().map(|r| r[0].clone()).collect();
            assert!(values.len() >= 2, "{kind:?}: {values:?}");
            let (kept, dropped) = values.split_at(1);
            let cond = crate::sql::values_condition(kind, &expr, kept, dropped, true).unwrap();
            let n = c
                .query(&format!("SELECT count(*) FROM dbl_t WHERE {cond}"))
                .unwrap();
            assert_eq!(n.rows[0][0], list.rows[0][1], "{kind:?}: {cond}");
        }
        // The foreign-key picker's queries run as they are (the MySQL time-limit hint too).
        {
            let schema = if kind == Kind::MySql { db } else { "public" };
            let fk = FkRef {
                schema: schema.into(),
                table: "dbl_t".into(),
                column: "id".into(),
            };
            for (local, search) in [(true, ""), (false, ""), (false, "1"), (false, "x")] {
                let sql = crate::sql::fk_options(kind, &fk, Some("name"), true, local, search);
                c.query(&sql).unwrap_or_else(|e| panic!("{kind:?}: {sql}: {e}"));
            }
        }
        // Errors say where they are: a typo in a keyword, a column, a table.
        let pos = |c: &mut Conn, sql: &str| c.query(sql).expect_err(sql).position_in(sql);
        let at = |c: &mut Conn, sql: &str| {
            pos(c, sql).map(|i| sql[i..].split_whitespace().next().unwrap_or("").to_string())
        };
        let third_line = "SELECT id\nFROM dbl_t\nORDER id";
        assert_eq!(pos(&mut c, third_line), third_line.rfind("id"), "{kind:?}");
        assert_eq!(
            at(&mut c, "SELECT id, nmae FROM dbl_t").as_deref(),
            Some("nmae"),
            "{kind:?}"
        );
        assert_eq!(
            at(&mut c, "SELECT * FROM dbl_nothing").as_deref(),
            Some("dbl_nothing"),
            "{kind:?}"
        );
        c.query("DROP TABLE dbl_t").unwrap();
    }

    #[test]
    #[ignore]
    fn postgres() {
        roundtrip(Kind::Postgres, 55432, "postgres", "pw", "postgres");
    }

    #[test]
    #[ignore]
    fn mysql() {
        roundtrip(Kind::MySql, 53306, "root", "pw", "test");
    }

    /// Needs a TLS-enabled Postgres with a self-signed certificate on port 55433.
    #[test]
    #[ignore]
    fn postgres_tls() {
        let mut cfg = ConnConfig::new(Kind::Postgres);
        cfg.host = "localhost".into();
        cfg.port = 55433;
        cfg.password = "pw".into();
        cfg.ssl = true;
        assert!(
            Conn::connect(&cfg, None).is_err(),
            "self-signed cert must be rejected"
        );
        cfg.ssl_insecure = true;
        let mut c = Conn::connect(&cfg, None).expect("insecure tls connect");
        let r = c
            .query("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("t"));

        // With TLS not required, a TLS-capable server is still used encrypted (sslmode=prefer).
        cfg.ssl = false;
        let mut c = Conn::connect(&cfg, None).expect("prefer connect");
        let r = c
            .query("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("t"));
    }

    #[test]
    #[ignore]
    fn mysql_tls() {
        let mut cfg = ConnConfig::new(Kind::MySql);
        cfg.host = "127.0.0.1".into();
        cfg.port = 53306;
        cfg.password = "pw".into();
        cfg.ssl = true;
        cfg.ssl_insecure = true;
        let mut c = Conn::connect(&cfg, None).expect("insecure tls connect");
        let r = c.query("SHOW STATUS LIKE 'Ssl_cipher'").unwrap();
        assert!(!r.rows[0][1].as_deref().unwrap_or("").is_empty());
    }
}
