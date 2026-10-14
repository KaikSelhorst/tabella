use std::time::{Duration, Instant};

use mysql::Value;
use mysql::prelude::Queryable;
use postgres::SimpleQueryMessage;
use serde::{Deserialize, Serialize};

use crate::error::{DbError, DbResult};
use crate::plan::{self, Plan};
use crate::sql::Stmt;
use crate::tls;

/// Rows kept in memory per result; protects against `SELECT *` on huge tables.
/// Most rows kept from one query.
fn max_rows() -> usize {
    5000
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Kind {
    Postgres,
    MySql,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Postgres => "PostgreSQL",
            Kind::MySql => "MySQL",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Kind::Postgres => 5432,
            Kind::MySql => 3306,
        }
    }

    pub fn quote_ident(self, s: &str) -> String {
        match self {
            Kind::Postgres => format!("\"{}\"", s.replace('"', "\"\"")),
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
            Kind::Postgres => format!("'{}'", s.replace('\'', "''")),
            Kind::MySql => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''")),
        }
    }

    /// `schema.table` fully quoted. In MySQL the "schema" is the database.
    pub fn qualified(self, schema: &str, table: &str) -> String {
        format!("{}.{}", self.quote_ident(schema), self.quote_ident(table))
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
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
            Tag::Red => "Vermelho",
            Tag::Orange => "Laranja",
            Tag::Yellow => "Amarelo",
            Tag::Green => "Verde",
            Tag::Blue => "Azul",
            Tag::Violet => "Violeta",
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

impl ConnConfig {
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
            host: "localhost".into(),
            port: kind.default_port(),
            user: match kind {
                Kind::Postgres => "postgres".into(),
                Kind::MySql => "root".into(),
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
            ObjKind::Function => "Funções",
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
    fn next_set(&mut self) {
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
        }
    }
}

/// What the "Estrutura" tab shows.
#[derive(Clone, Debug, Default)]
pub struct TableStructure {
    pub columns: Vec<StructColumn>,
    /// `(name, definition)`.
    pub indexes: Vec<(String, String)>,
    /// `(name, definition)`.
    pub foreign_keys: Vec<(String, String)>,
    /// `CREATE TABLE ...` (plus separate indexes on Postgres), ready to copy.
    pub ddl: String,
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
}

impl Conn {
    /// Connects to `cfg`, or through a local tunnel port when `via` is set (SSH).
    pub fn connect(cfg: &ConnConfig, via: Option<u16>) -> DbResult<Self> {
        let mut conn = Self::open(cfg, via)?;
        if cfg.read_only {
            // Enforced by the server, so the SQL editor can't write either.
            conn.query(match cfg.kind {
                Kind::Postgres => "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
                Kind::MySql => "SET SESSION TRANSACTION READ ONLY",
            })?;
        }
        if cfg.timeout_secs > 0 {
            // Server-side limit: also covers COUNT(*) and exports, not just the SQL editor.
            // MySQL's max_execution_time only applies to SELECT statements.
            let ms = u64::from(cfg.timeout_secs) * 1000;
            conn.query(&match cfg.kind {
                Kind::Postgres => format!("SET statement_timeout = {ms}"),
                Kind::MySql => format!("SET SESSION max_execution_time = {ms}"),
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
        }
    }

    /// A handle that can stop the statement this connection is running, from another thread.
    pub fn canceller(&self, cfg: &ConnConfig, via: Option<u16>) -> Canceller {
        match self {
            Conn::Pg(c) => Canceller::Pg(c.cancel_token(), cfg.ssl_insecure || !cfg.ssl),
            Conn::My(c) => Canceller::My(Box::new(my_opts(cfg, via)), c.connection_id()),
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Conn::Pg(_) => Kind::Postgres,
            Conn::My(_) => Kind::MySql,
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
        };
        out.elapsed = start.elapsed();
        Ok(out)
    }

    fn query_limit(&mut self, sql: &str, limit: usize) -> DbResult<QueryResult> {
        let start = Instant::now();
        let mut out = match self {
            Conn::Pg(c) => pg_query(c, sql, limit)?,
            Conn::My(c) => my_query(c, sql, limit)?,
        };
        out.elapsed = start.elapsed();
        Ok(out)
    }

    pub fn tables(&mut self) -> DbResult<Vec<TableRef>> {
        let sql = match self {
            Conn::Pg(_) => {
                "SELECT t.table_schema, t.table_name, t.table_type, c.reltuples::bigint, \
                 CASE WHEN c.relkind IN ('r', 'p', 'm') THEN pg_total_relation_size(c.oid) END \
                 FROM information_schema.tables t \
                 LEFT JOIN pg_catalog.pg_namespace n ON n.nspname = t.table_schema \
                 LEFT JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = t.table_name \
                 WHERE t.table_schema NOT IN ('pg_catalog', 'information_schema') \
                 AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = c.oid AND d.deptype = 'e') \
                 ORDER BY 1, 2"
            }
            Conn::My(_) => {
                "SELECT table_schema, table_name, table_type, table_rows, data_length + index_length \
                 FROM information_schema.tables \
                 WHERE table_schema NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') \
                 ORDER BY 1, 2"
            }
        };
        let r = self.query(sql)?;
        r.rows
            .into_iter()
            .map(|row| {
                let [schema, name, ty, rows, size] = cells(row)?;
                let is_view = ty.contains("VIEW");
                Ok(TableRef {
                    size: size.parse().ok().filter(|_| !is_view),
                    search_key: name.to_lowercase(),
                    schema,
                    name,
                    is_view,
                    // Postgres reports -1 for tables that were never analyzed.
                    estimate: rows.parse().ok().filter(|n: &i64| *n >= 0 && !is_view),
                })
            })
            .collect()
    }

    /// Every `(table, column)` outside system schemas, for editor autocomplete.
    /// Every column of every table, for autocomplete, the schema search and the ER diagram.
    pub fn all_columns(&mut self) -> DbResult<Vec<SchemaColumn>> {
        let system = match self {
            Conn::Pg(_) => "'pg_catalog', 'information_schema'",
            Conn::My(_) => "'information_schema', 'mysql', 'performance_schema', 'sys'",
        };
        let sql = match self {
            Conn::Pg(_) => format!(
                "SELECT c.table_schema, c.table_name, c.column_name, c.data_type, \
                 CASE WHEN pk.col IS NULL THEN '0' ELSE '1' END \
                 FROM information_schema.columns c \
                 LEFT JOIN (SELECT n.nspname AS s, cl.relname AS t, a.attname AS col \
                   FROM pg_index i JOIN pg_class cl ON cl.oid = i.indrelid \
                   JOIN pg_namespace n ON n.oid = cl.relnamespace \
                   JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY (i.indkey) \
                   WHERE i.indisprimary) pk \
                   ON pk.s = c.table_schema AND pk.t = c.table_name AND pk.col = c.column_name \
                 WHERE c.table_schema NOT IN ({system}) ORDER BY 1, 2, c.ordinal_position"
            ),
            Conn::My(_) => format!(
                "SELECT table_schema, table_name, column_name, data_type, IF(column_key = 'PRI', '1', '0') \
                 FROM information_schema.columns \
                 WHERE table_schema NOT IN ({system}) ORDER BY 1, 2, ordinal_position"
            ),
        };
        let r = self.query_limit(&sql, 100_000)?;
        r.rows
            .into_iter()
            .map(|row| {
                let [schema, table, column, data_type, pk] = cells(row)?;
                Ok(SchemaColumn {
                    schema,
                    table,
                    column,
                    data_type,
                    pk: pk == "1",
                })
            })
            .collect()
    }

    /// Exact row count for `SELECT COUNT(*) ...`.
    pub fn count(&mut self, sql: &str) -> DbResult<i64> {
        let r = self.query(sql)?;
        let [n] = cells(r.rows.into_iter().next().unwrap_or_default())?;
        n.parse()
            .map_err(|_| DbError::Other(format!("contagem inválida: {n}")))
    }

    pub fn columns(&mut self, schema: &str, table: &str) -> DbResult<Vec<ColumnInfo>> {
        let stmt = match self.kind() {
            Kind::Postgres => Stmt::new(
                "SELECT c.column_name, c.data_type, EXISTS (\
                   SELECT 1 FROM information_schema.table_constraints tc \
                   JOIN information_schema.key_column_usage u \
                     USING (constraint_schema, constraint_name, table_schema, table_name) \
                   WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = c.table_schema \
                     AND tc.table_name = c.table_name AND u.column_name = c.column_name), \
                 c.is_nullable, \
                 c.column_default IS NOT NULL OR c.is_identity = 'YES' OR c.is_generated = 'ALWAYS' \
                 FROM information_schema.columns c WHERE c.table_schema = ",
            ),
            Kind::MySql => Stmt::new(
                "SELECT column_name, column_type, column_key = 'PRI', is_nullable, \
                 column_default IS NOT NULL OR extra LIKE '%auto_increment%' OR extra LIKE '%GENERATED%' \
                 FROM information_schema.columns WHERE table_schema = ",
            ),
        };
        let order = match self.kind() {
            Kind::Postgres => " ORDER BY c.ordinal_position",
            Kind::MySql => " ORDER BY ordinal_position",
        };
        let table_col = match self.kind() {
            Kind::Postgres => " AND c.table_name = ",
            Kind::MySql => " AND table_name = ",
        };
        let stmt = stmt
            .value(Some(schema))
            .sql(table_col)
            .value(Some(table))
            .sql(order);
        let r = self.execute(&stmt)?;
        r.rows
            .into_iter()
            .map(|row| {
                let [name, data_type, pk, nullable, has_default] = cells(row)?;
                let yes = |v: &str| matches!(v, "t" | "1" | "true");
                Ok(ColumnInfo {
                    name,
                    data_type,
                    pk: yes(&pk),
                    nullable: nullable == "YES",
                    has_default: yes(&has_default),
                    references: None,
                })
            })
            .collect()
    }
}

impl Conn {
    /// Columns of a table with their foreign keys filled in (single-column keys only: those are
    /// the ones a cell value can be followed through).
    pub fn columns_with_fks(&mut self, schema: &str, table: &str) -> DbResult<Vec<ColumnInfo>> {
        let mut cols = self.columns(schema, table)?;
        let stmt = match self.kind() {
            Kind::Postgres => Stmt::new(
                "SELECT a.attname, fn.nspname, fc.relname, fa.attname \
                 FROM pg_constraint k \
                 JOIN pg_class c ON c.oid = k.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_class fc ON fc.oid = k.confrelid JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
                 JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = k.conkey[1] \
                 JOIN pg_attribute fa ON fa.attrelid = k.confrelid AND fa.attnum = k.confkey[1] \
                 WHERE k.contype = 'f' AND array_length(k.conkey, 1) = 1 AND n.nspname = ",
            )
            .value(Some(schema))
            .sql(" AND c.relname = ")
            .value(Some(table)),
            Kind::MySql => Stmt::new(
                "SELECT k.column_name, k.referenced_table_schema, k.referenced_table_name, k.referenced_column_name \
                 FROM information_schema.key_column_usage k \
                 WHERE k.referenced_table_name IS NOT NULL AND k.table_schema = ",
            )
            .value(Some(schema))
            .sql(" AND k.table_name = ")
            .value(Some(table))
            .sql(
                " AND (SELECT COUNT(*) FROM information_schema.key_column_usage k2 \
                 WHERE k2.constraint_schema = k.constraint_schema AND k2.constraint_name = k.constraint_name \
                 AND k2.table_name = k.table_name) = 1",
            ),
        };
        for row in self.execute(&stmt)?.rows {
            let [col, s, t, c] = cells(row)?;
            if let Some(info) = cols.iter_mut().find(|i| i.name == col) {
                info.references = Some(FkRef {
                    schema: s,
                    table: t,
                    column: c,
                });
            }
        }
        Ok(cols)
    }

    /// Where unqualified names go now, and where else they could: `(databases, current database,
    /// current schema)`. MySQL has no schemas inside a database (the schema is empty).
    pub fn location(&mut self) -> DbResult<(Vec<String>, String, String)> {
        let (list, here) = match self.kind() {
            Kind::Postgres => (
                "SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate ORDER BY 1",
                "SELECT current_database(), coalesce(current_schema(), '')",
            ),
            Kind::MySql => (
                "SELECT schema_name FROM information_schema.schemata \
                 WHERE schema_name NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') ORDER BY 1",
                "SELECT coalesce(DATABASE(), ''), ''",
            ),
        };
        let dbs = self
            .query(list)?
            .rows
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten())
            .collect();
        let [db, schema] = cells(self.query(here)?.rows.into_iter().next().unwrap_or_default())?;
        Ok((dbs, db, schema))
    }

    /// Unqualified names from now on: MySQL `USE db`; Postgres the schema first in
    /// `search_path` (public stays after it).
    pub fn use_schema(&mut self, name: &str) -> DbResult<()> {
        let k = self.kind();
        let sql = match k {
            Kind::MySql => format!("USE {}", k.quote_ident(name)),
            Kind::Postgres if name == "public" => "SET search_path TO public".to_string(),
            Kind::Postgres => format!("SET search_path TO {}, public", k.quote_ident(name)),
        };
        self.query(&sql).map(|_| ())
    }

    /// Functions, procedures, triggers, views and sequences outside the system schemas (and not
    /// installed by extensions).
    pub fn objects(&mut self) -> DbResult<Vec<DbObject>> {
        let rows: Vec<[String; 6]> = match self.kind() {
            Kind::Postgres => {
                let sql = "SELECT n.nspname, p.proname, CASE p.prokind WHEN 'p' THEN 'p' ELSE 'f' END, \
                     pg_get_function_identity_arguments(p.oid), '', p.oid::text \
                     FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                     WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND p.prokind IN ('f', 'p') \
                     AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e') \
                     UNION ALL \
                     SELECT n.nspname, t.tgname, 't', '', c.relname, '' \
                     FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE NOT t.tgisinternal AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
                     UNION ALL \
                     SELECT schemaname, viewname, 'v', '', '', '' FROM pg_views v \
                     WHERE schemaname NOT IN ('pg_catalog', 'information_schema') \
                     AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.deptype = 'e' \
                       AND d.objid = (quote_ident(v.schemaname) || '.' || quote_ident(v.viewname))::regclass) \
                     UNION ALL \
                     SELECT schemaname, sequencename, 's', '', '', '' FROM pg_sequences \
                     ORDER BY 1, 3, 2";
                self.query_limit(sql, 100_000)?
                    .rows
                    .into_iter()
                    .map(cells)
                    .collect::<DbResult<_>>()?
            }
            Kind::MySql => {
                let sql = "SELECT routine_schema, routine_name, IF(routine_type = 'PROCEDURE', 'p', 'f'), \
                     coalesce((SELECT GROUP_CONCAT(CONCAT(coalesce(p.parameter_mode, 'IN'), ' ', p.parameter_name, ' ', \
                       p.dtd_identifier) ORDER BY p.ordinal_position SEPARATOR ', ') \
                       FROM information_schema.parameters p WHERE p.specific_schema = r.routine_schema \
                       AND p.specific_name = r.routine_name AND p.ordinal_position > 0), ''), '', '' \
                     FROM information_schema.routines r \
                     WHERE routine_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                     UNION ALL \
                     SELECT trigger_schema, trigger_name, 't', '', event_object_table, '' FROM information_schema.triggers \
                     WHERE trigger_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                     UNION ALL \
                     SELECT table_schema, table_name, 'v', '', '', '' FROM information_schema.views \
                     WHERE table_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                     ORDER BY 1, 3, 2";
                self.query_limit(sql, 100_000)?
                    .rows
                    .into_iter()
                    .map(cells)
                    .collect::<DbResult<_>>()?
            }
        };
        Ok(rows
            .into_iter()
            .map(|[schema, name, kind, args, table, id]| DbObject {
                schema,
                name,
                kind: match kind.as_str() {
                    "p" => ObjKind::Procedure,
                    "t" => ObjKind::Trigger,
                    "v" => ObjKind::View,
                    "s" => ObjKind::Sequence,
                    _ => ObjKind::Function,
                },
                args,
                table,
                id,
            })
            .collect())
    }

    /// The object's definition as SQL that recreates it (CREATE OR REPLACE where the engine has it).
    pub fn object_source(&mut self, o: &DbObject) -> DbResult<String> {
        let first_cell = |r: QueryResult, col: usize| -> DbResult<String> {
            r.rows
                .into_iter()
                .next()
                .and_then(|row| row.into_iter().nth(col).flatten())
                .ok_or_else(|| DbError::Other(format!("não encontrei a definição de {}", o.name)))
        };
        let k = self.kind();
        match k {
            Kind::Postgres => {
                // Names go in as values; the server quotes them into a regprocedure / regclass.
                let qualified = |name: &str| {
                    Stmt::new("quote_ident(")
                        .value(Some(&o.schema))
                        .sql(") || '.' || quote_ident(")
                        .value(Some(name))
                        .sql(")")
                };
                let stmt = match o.kind {
                    ObjKind::Function | ObjKind::Procedure => Stmt::new("SELECT pg_get_functiondef(CAST(")
                        .value(Some(&o.id))
                        .sql(" AS oid))"),
                    ObjKind::Trigger => Stmt::new(
                        "SELECT pg_get_triggerdef(t.oid, true) || ';' FROM pg_trigger t \
                         JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                         WHERE n.nspname = ",
                    )
                    .value(Some(&o.schema))
                    .sql(" AND c.relname = ")
                    .value(Some(&o.table))
                    .sql(" AND t.tgname = ")
                    .value(Some(&o.name)),
                    ObjKind::View => Stmt::new("SELECT 'CREATE OR REPLACE VIEW ' || ")
                        .sql(qualified(&o.name).render(k))
                        .sql(" || E' AS\\n' || pg_get_viewdef((")
                        .sql(qualified(&o.name).render(k))
                        .sql(")::regclass, true)"),
                    ObjKind::Sequence => Stmt::new(
                        "SELECT 'CREATE SEQUENCE ' || quote_ident(schemaname) || '.' || quote_ident(sequencename) \
                         || E'\\n  INCREMENT BY ' || increment_by || E'\\n  MINVALUE ' || min_value \
                         || E'\\n  MAXVALUE ' || max_value || E'\\n  START WITH ' || start_value \
                         || E';\\n-- valor atual: ' || coalesce(last_value::text, 'ainda não usada') \
                         FROM pg_sequences WHERE schemaname = ",
                    )
                    .value(Some(&o.schema))
                    .sql(" AND sequencename = ")
                    .value(Some(&o.name)),
                };
                first_cell(self.execute(&stmt)?, 0)
            }
            Kind::MySql => {
                let what = match o.kind {
                    ObjKind::Function => "FUNCTION",
                    ObjKind::Procedure => "PROCEDURE",
                    ObjKind::Trigger => "TRIGGER",
                    ObjKind::View => "VIEW",
                    ObjKind::Sequence => return Err(DbError::Other("MySQL não tem sequences".into())),
                };
                let r = self.query(&format!("SHOW CREATE {what} {}", k.qualified(&o.schema, &o.name)))?;
                // The definition column: 2nd for views, 3rd for routines and triggers.
                let col = if o.kind == ObjKind::View { 1 } else { 2 };
                let body = first_cell(r, col)?;
                // MySQL has no CREATE OR REPLACE for these: dropping first is left to the user.
                // The body has its own ";" inside, so it runs with F5 (the whole text), not
                // Ctrl+Enter.
                Ok(match o.kind {
                    ObjKind::View => format!("{};", body.replacen("CREATE ", "CREATE OR REPLACE ", 1)),
                    _ => format!(
                        "-- Para recriar: descomente o DROP e execute tudo (F5).\n-- DROP {what} IF EXISTS {};\n{body}",
                        k.qualified(&o.schema, &o.name)
                    ),
                })
            }
        }
    }

    /// Every single-column foreign key of the connection (for the ER diagram).
    pub fn all_fks(&mut self) -> DbResult<Vec<FkEdge>> {
        let sql = match self.kind() {
            Kind::Postgres => {
                "SELECT n.nspname, c.relname, a.attname, fn.nspname, fc.relname, fa.attname \
                 FROM pg_constraint k \
                 JOIN pg_class c ON c.oid = k.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_class fc ON fc.oid = k.confrelid JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
                 JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = k.conkey[1] \
                 JOIN pg_attribute fa ON fa.attrelid = k.confrelid AND fa.attnum = k.confkey[1] \
                 WHERE k.contype = 'f' AND array_length(k.conkey, 1) = 1 \
                 AND n.nspname NOT IN ('pg_catalog', 'information_schema') ORDER BY 1, 2, 3"
            }
            Kind::MySql => {
                "SELECT k.table_schema, k.table_name, k.column_name, \
                 k.referenced_table_schema, k.referenced_table_name, k.referenced_column_name \
                 FROM information_schema.key_column_usage k \
                 WHERE k.referenced_table_name IS NOT NULL \
                 AND k.table_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                 AND (SELECT COUNT(*) FROM information_schema.key_column_usage k2 \
                  WHERE k2.constraint_schema = k.constraint_schema AND k2.constraint_name = k.constraint_name \
                  AND k2.table_name = k.table_name) = 1 ORDER BY 1, 2, 3"
            }
        };
        self.query_limit(sql, 100_000)?
            .rows
            .into_iter()
            .map(|row| {
                let [schema, table, column, ref_schema, ref_table, ref_column] = cells(row)?;
                Ok(FkEdge {
                    schema,
                    table,
                    column,
                    ref_schema,
                    ref_table,
                    ref_column,
                })
            })
            .collect()
    }

    /// Single-column foreign keys in other tables that point at this one ("quem aponta para cá").
    pub fn incoming_fks(&mut self, schema: &str, table: &str) -> DbResult<Vec<IncomingFk>> {
        let stmt = match self.kind() {
            Kind::Postgres => Stmt::new(
                "SELECT n.nspname, c.relname, a.attname, fa.attname \
                 FROM pg_constraint k \
                 JOIN pg_class c ON c.oid = k.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_class fc ON fc.oid = k.confrelid JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
                 JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = k.conkey[1] \
                 JOIN pg_attribute fa ON fa.attrelid = k.confrelid AND fa.attnum = k.confkey[1] \
                 WHERE k.contype = 'f' AND array_length(k.conkey, 1) = 1 AND fn.nspname = ",
            )
            .value(Some(schema))
            .sql(" AND fc.relname = ")
            .value(Some(table))
            .sql(" ORDER BY 1, 2, 3"),
            Kind::MySql => Stmt::new(
                "SELECT k.table_schema, k.table_name, k.column_name, k.referenced_column_name \
                 FROM information_schema.key_column_usage k \
                 WHERE k.referenced_table_schema = ",
            )
            .value(Some(schema))
            .sql(" AND k.referenced_table_name = ")
            .value(Some(table))
            .sql(
                " AND (SELECT COUNT(*) FROM information_schema.key_column_usage k2 \
                 WHERE k2.constraint_schema = k.constraint_schema AND k2.constraint_name = k.constraint_name \
                 AND k2.table_name = k.table_name) = 1 ORDER BY 1, 2, 3",
            ),
        };
        self.execute(&stmt)?
            .rows
            .into_iter()
            .map(|row| {
                let [schema, table, column, target] = cells(row)?;
                Ok(IncomingFk {
                    schema,
                    table,
                    column,
                    target,
                })
            })
            .collect()
    }

    /// One of the activity monitor's views. In every view where something can be cancelled,
    /// the first column is the session id to act on.
    pub fn activity_view(&mut self, view: ActivityView) -> DbResult<QueryResult> {
        match view {
            ActivityView::Sessions | ActivityView::Server => self.activity(),
            ActivityView::Locks => self.query(match self.kind() {
                // Who waits for whom: the blocking session first (that's the one to stop).
                Kind::Postgres => {
                    "SELECT b.pid, b.usename AS usuario, b.state AS estado, left(b.query, 200) AS consulta_que_bloqueia, \
                     w.pid AS esperando, date_trunc('second', now() - w.query_start)::text AS esperando_ha, \
                     left(w.query, 200) AS consulta_esperando \
                     FROM pg_stat_activity w \
                     JOIN LATERAL unnest(pg_blocking_pids(w.pid)) AS x(pid) ON true \
                     JOIN pg_stat_activity b ON b.pid = x.pid \
                     ORDER BY w.query_start"
                }
                Kind::MySql => {
                    "SELECT blocking_pid AS pid, blocking_query AS consulta_que_bloqueia, \
                     waiting_pid AS esperando, wait_age AS esperando_ha, waiting_query AS consulta_esperando \
                     FROM sys.innodb_lock_waits ORDER BY wait_age_secs DESC"
                }
            }),
            ActivityView::Slow => {
                let r = self.query(match self.kind() {
                    Kind::Postgres => {
                        "SELECT calls AS chamadas, round(total_exec_time::numeric, 1) AS total_ms, \
                         round(mean_exec_time::numeric, 2) AS media_ms, rows AS linhas, left(query, 500) AS consulta \
                         FROM pg_stat_statements \
                         WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
                         ORDER BY total_exec_time DESC LIMIT 50"
                    }
                    Kind::MySql => {
                        "SELECT COUNT_STAR AS chamadas, ROUND(SUM_TIMER_WAIT / 1e9, 1) AS total_ms, \
                         ROUND(AVG_TIMER_WAIT / 1e9, 2) AS media_ms, SUM_ROWS_EXAMINED AS linhas_lidas, \
                         SCHEMA_NAME AS banco, LEFT(DIGEST_TEXT, 500) AS consulta \
                         FROM performance_schema.events_statements_summary_by_digest \
                         WHERE DIGEST_TEXT IS NOT NULL ORDER BY SUM_TIMER_WAIT DESC LIMIT 50"
                    }
                });
                r.map_err(|e| match self.kind() {
                    Kind::Postgres if e.to_string().contains("pg_stat_statements") => DbError::Other(
                        "Consultas lentas usam a extensão pg_stat_statements: inclua-a em \
                         shared_preload_libraries (postgresql.conf), reinicie o servidor e rode \
                         CREATE EXTENSION pg_stat_statements; no banco."
                            .into(),
                    ),
                    _ => e,
                })
            }
        }
    }

    /// The "Servidor" panel: headline numbers, biggest tables, indexes nobody uses.
    pub fn server_info(&mut self) -> DbResult<ServerInfo> {
        let (facts_sql, largest, indexes) = match self.kind() {
            Kind::Postgres => (
                "SELECT current_setting('server_version'), \
                 date_trunc('second', now() - pg_postmaster_start_time())::text, \
                 pg_size_pretty(pg_database_size(current_database())), \
                 (SELECT count(*) FROM pg_stat_activity)::text || ' de ' || current_setting('max_connections'), \
                 (SELECT round(100.0 * sum(blks_hit) / nullif(sum(blks_hit) + sum(blks_read), 0), 2)::text || '%' \
                  FROM pg_stat_database)",
                "SELECT n.nspname || '.' || c.relname AS tabela, pg_size_pretty(pg_total_relation_size(c.oid)) AS total, \
                 pg_size_pretty(pg_relation_size(c.oid)) AS dados, pg_size_pretty(pg_indexes_size(c.oid)) AS indices, \
                 c.reltuples::bigint AS linhas_est \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relkind IN ('r', 'p', 'm') AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
                 AND n.nspname NOT LIKE 'pg_toast%' \
                 ORDER BY pg_total_relation_size(c.oid) DESC LIMIT 10",
                "SELECT s.schemaname || '.' || s.relname AS tabela, s.indexrelname AS indice, \
                 s.idx_scan AS usos, pg_size_pretty(pg_relation_size(s.indexrelid)) AS tamanho, \
                 CASE WHEN EXISTS (SELECT 1 FROM pg_constraint c WHERE c.contype = 'f' \
                   AND c.conrelid = i.indrelid AND c.conkey[1] = i.indkey[0]) THEN 'sim' ELSE '' END \
                   AS apoia_chave_estrangeira \
                 FROM pg_stat_user_indexes s JOIN pg_index i ON i.indexrelid = s.indexrelid \
                 WHERE NOT i.indisunique AND NOT i.indisprimary \
                 ORDER BY s.idx_scan, pg_relation_size(s.indexrelid) DESC LIMIT 20",
            ),
            Kind::MySql => (
                "SELECT VERSION(), \
                 (SELECT CONCAT(FLOOR(VARIABLE_VALUE / 86400), 'd ', SEC_TO_TIME(VARIABLE_VALUE % 86400)) \
                  FROM performance_schema.global_status WHERE VARIABLE_NAME = 'Uptime'), \
                 (SELECT CONCAT(ROUND(SUM(data_length + index_length) / 1048576, 1), ' MB') \
                  FROM information_schema.tables WHERE table_schema = DATABASE()), \
                 CONCAT((SELECT VARIABLE_VALUE FROM performance_schema.global_status WHERE VARIABLE_NAME = 'Threads_connected'), \
                  ' de ', @@max_connections), \
                 (SELECT CONCAT(ROUND(100 * (1 - r.VARIABLE_VALUE / NULLIF(q.VARIABLE_VALUE, 0)), 2), '%') \
                  FROM performance_schema.global_status r, performance_schema.global_status q \
                  WHERE r.VARIABLE_NAME = 'Innodb_buffer_pool_reads' AND q.VARIABLE_NAME = 'Innodb_buffer_pool_read_requests')",
                "SELECT CONCAT(table_schema, '.', table_name) AS tabela, \
                 CONCAT(ROUND((data_length + index_length) / 1048576, 1), ' MB') AS total, \
                 CONCAT(ROUND(data_length / 1048576, 1), ' MB') AS dados, \
                 CONCAT(ROUND(index_length / 1048576, 1), ' MB') AS indices, table_rows AS linhas_est \
                 FROM information_schema.tables \
                 WHERE table_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                 AND table_type = 'BASE TABLE' \
                 ORDER BY data_length + index_length DESC LIMIT 10",
                // performance_schema only knows tables opened since its counters started: an
                // index with no row there shows NULL ("no data"), not 0.
                "SELECT CONCAT(s.table_schema, '.', s.table_name) AS tabela, s.index_name AS indice, \
                 u.COUNT_READ AS usos, \
                 IF(EXISTS (SELECT 1 FROM information_schema.key_column_usage k \
                   WHERE k.table_schema = s.table_schema AND k.table_name = s.table_name \
                   AND k.column_name = s.column_name AND k.referenced_table_name IS NOT NULL), 'sim', '') \
                   AS apoia_chave_estrangeira \
                 FROM information_schema.statistics s \
                 LEFT JOIN performance_schema.table_io_waits_summary_by_index_usage u \
                   ON u.OBJECT_SCHEMA = s.table_schema AND u.OBJECT_NAME = s.table_name AND u.INDEX_NAME = s.index_name \
                 WHERE s.seq_in_index = 1 AND s.non_unique = 1 \
                 AND s.table_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                 ORDER BY u.COUNT_READ IS NOT NULL, u.COUNT_READ, s.table_name LIMIT 20",
            ),
        };
        let facts = self.query(facts_sql)?;
        let labels = [
            "Versão",
            "No ar há",
            "Tamanho do banco",
            "Conexões",
            "Acerto do cache",
        ];
        let values = facts.rows.into_iter().next().unwrap_or_default();
        Ok(ServerInfo {
            facts: labels
                .iter()
                .zip(values)
                // Postgres prints intervals in English ("3 days 04:12:09"): same form as MySQL's.
                .map(|(l, v)| {
                    let v = v.unwrap_or_else(|| "—".into());
                    (l.to_string(), v.replace(" days ", "d ").replace(" day ", "d "))
                })
                .collect(),
            largest: self.query(largest)?,
            indexes: self.query(indexes)?,
        })
    }

    /// Clears the "consultas lentas" counters.
    pub fn reset_query_stats(&mut self) -> DbResult<()> {
        self.query(match self.kind() {
            Kind::Postgres => "SELECT pg_stat_statements_reset()",
            Kind::MySql => "TRUNCATE TABLE performance_schema.events_statements_summary_by_digest",
        })
        .map(|_| ())
    }

    /// Sessions on the server right now (excluding this one), most long-running first.
    pub fn activity(&mut self) -> DbResult<QueryResult> {
        self.query(match self.kind() {
            Kind::Postgres => {
                "SELECT pid, usename AS usuario, datname AS banco, state AS estado, \
                 date_trunc('second', now() - query_start)::text AS duracao, \
                 wait_event_type AS espera, client_addr AS cliente, left(query, 300) AS consulta \
                 FROM pg_stat_activity \
                 WHERE pid <> pg_backend_pid() AND backend_type = 'client backend' \
                 ORDER BY state = 'active' DESC, query_start NULLS LAST"
            }
            Kind::MySql => {
                "SELECT id, user AS usuario, db AS banco, command AS estado, \
                 SEC_TO_TIME(time) AS duracao, state AS espera, host AS cliente, LEFT(info, 300) AS consulta \
                 FROM information_schema.processlist \
                 WHERE id <> CONNECTION_ID() AND command <> 'Daemon' \
                 ORDER BY command = 'Query' DESC, time DESC"
            }
        })
    }

    /// Plan of one statement. Postgres: `EXPLAIN (FORMAT JSON)`; MySQL: `EXPLAIN FORMAT=TREE`, or
    /// `EXPLAIN ANALYZE` (8.0.18+). With `analyze` the statement really runs: callers only allow
    /// it for reads.
    pub fn explain(&mut self, sql: &str, analyze: bool) -> DbResult<Plan> {
        let sql = sql.trim().trim_end_matches(';');
        let (q, kind) = match (self.kind(), analyze) {
            (Kind::Postgres, false) => (format!("EXPLAIN (FORMAT JSON) {sql}"), Kind::Postgres),
            (Kind::Postgres, true) => (format!("EXPLAIN (ANALYZE, FORMAT JSON) {sql}"), Kind::Postgres),
            (Kind::MySql, false) => (format!("EXPLAIN FORMAT=TREE {sql}"), Kind::MySql),
            (Kind::MySql, true) => (format!("EXPLAIN ANALYZE {sql}"), Kind::MySql),
        };
        let r = self.query(&q)?;
        // Postgres returns one JSON cell; MySQL one text cell (or one line per row on old servers).
        let text: Vec<&str> = r.rows.iter().filter_map(|row| row.first()?.as_deref()).collect();
        let text = text.join("\n");
        match kind {
            Kind::Postgres => plan::from_pg_json(&text),
            Kind::MySql => plan::from_mysql_tree(&text),
        }
        .map_err(DbError::Other)
    }

    /// Stops another session's statement (`terminate` = close that whole connection).
    pub fn kill(&mut self, id: i64, terminate: bool) -> DbResult<()> {
        let sql = match (self.kind(), terminate) {
            (Kind::Postgres, false) => format!("SELECT pg_cancel_backend({id})"),
            (Kind::Postgres, true) => format!("SELECT pg_terminate_backend({id})"),
            (Kind::MySql, false) => format!("KILL QUERY {id}"),
            (Kind::MySql, true) => format!("KILL {id}"),
        };
        let r = self.query(&sql)?;
        // Postgres answers `false` instead of failing when the session is gone or belongs to
        // someone this user may not signal.
        if r.rows
            .first()
            .and_then(|row| row.first())
            .is_some_and(|v| v.as_deref() == Some("f"))
        {
            return Err(DbError::Other(format!(
                "a sessão {id} não foi {} (já terminou, ou falta permissão: é preciso ser o mesmo \
                 usuário, superusuário ou ter a role pg_signal_backend)",
                if terminate { "encerrada" } else { "cancelada" }
            )));
        }
        Ok(())
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
                            "A alteração {} não encontrou a linha (ela mudou ou foi excluída em outra sessão). Nada foi salvo.",
                            i + 1
                        )));
                    }
                    Err(e) => return Err(DbError::Other(format!("Alteração {}: {e}. Nada foi salvo.", i + 1))),
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
                "SAVEPOINT dblite_batch",
                "RELEASE SAVEPOINT dblite_batch",
                "ROLLBACK TO SAVEPOINT dblite_batch",
            ),
            (false, Kind::Postgres) => ("BEGIN", "COMMIT", "ROLLBACK"),
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
                "prévia indisponível para UPDATE/DELETE com JOIN ou várias tabelas no MySQL".into(),
            ));
        }
        let (begin, rollback) = if nested {
            ("SAVEPOINT dblite_preview", "ROLLBACK TO SAVEPOINT dblite_preview")
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
            Conn::My(_) => Err(DbError::Other("COPY … FROM stdin só existe no Postgres".into())),
        }
    }

    /// Opens a transaction for the "commit manual" mode.
    pub fn begin(&mut self) -> DbResult<()> {
        self.query(match self.kind() {
            Kind::Postgres => "BEGIN",
            Kind::MySql => "START TRANSACTION",
        })
        .map(|_| ())
    }

    pub fn structure(&mut self, schema: &str, table: &str) -> DbResult<TableStructure> {
        match self.kind() {
            Kind::Postgres => self.pg_structure(schema, table),
            Kind::MySql => self.my_structure(schema, table),
        }
    }

    fn pg_structure(&mut self, schema: &str, table: &str) -> DbResult<TableStructure> {
        let k = Kind::Postgres;
        // The table's oid, from the names as values (never spliced into the SQL text).
        let rel = |sql: &str| {
            Stmt::new(sql)
                .sql("(quote_ident(")
                .value(Some(schema))
                .sql(") || '.' || quote_ident(")
                .value(Some(table))
                .sql("))::regclass")
        };
        let cols = self.execute(
            &rel(
                "SELECT a.attname, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
             pg_get_expr(d.adbin, d.adrelid), \
             EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary \
                     AND a.attnum = ANY (i.indkey)) \
             FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attnum > 0 AND NOT a.attisdropped AND a.attrelid = ",
            )
            .sql(" ORDER BY a.attnum"),
        )?;
        let columns: Vec<StructColumn> = cols
            .rows
            .into_iter()
            .map(|r| StructColumn {
                name: r[0].clone().unwrap_or_default(),
                data_type: r[1].clone().unwrap_or_default(),
                nullable: r[2].as_deref() == Some("t"),
                default: r[3].clone(),
                pk: r[4].as_deref() == Some("t"),
            })
            .collect();
        let cons = self.execute(&rel(
            "SELECT conname, contype::text, pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = ",
        ).sql(" ORDER BY CASE contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'f' THEN 2 ELSE 3 END, conname"))?;
        let idx = self.execute(
            &rel("SELECT i.relname, pg_get_indexdef(i.oid), \
             EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = i.oid) \
             FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid WHERE x.indrelid = ")
            .sql(" ORDER BY 1"),
        )?;
        let cell = |r: &Vec<Option<String>>, i: usize| r.get(i).cloned().flatten().unwrap_or_default();

        let mut lines: Vec<String> = columns
            .iter()
            .map(|c| {
                let mut l = format!("    {} {}", k.quote_ident(&c.name), c.data_type);
                if !c.nullable {
                    l += " NOT NULL";
                }
                if let Some(d) = &c.default {
                    l += &format!(" DEFAULT {d}");
                }
                l
            })
            .collect();
        for r in &cons.rows {
            lines.push(format!(
                "    CONSTRAINT {} {}",
                k.quote_ident(&cell(r, 0)),
                cell(r, 2)
            ));
        }
        let mut ddl = format!(
            "CREATE TABLE {} (\n{}\n);",
            k.qualified(schema, table),
            lines.join(",\n")
        );
        // Indexes backing a constraint are created by it; the rest need their own statement.
        for r in idx.rows.iter().filter(|r| r[2].as_deref() != Some("t")) {
            ddl += &format!("\n{};", cell(r, 1));
        }
        Ok(TableStructure {
            columns,
            indexes: idx.rows.iter().map(|r| (cell(r, 0), cell(r, 1))).collect(),
            foreign_keys: cons
                .rows
                .iter()
                .filter(|r| r[1].as_deref() == Some("f"))
                .map(|r| (cell(r, 0), cell(r, 2)))
                .collect(),
            ddl,
        })
    }

    fn my_structure(&mut self, schema: &str, table: &str) -> DbResult<TableStructure> {
        let filter = |sql: &str| {
            Stmt::new(sql)
                .value(Some(schema))
                .sql(" AND table_name = ")
                .value(Some(table))
        };
        let cols = self.execute(&filter(
            "SELECT column_name, column_type, is_nullable = 'YES', column_default, column_key = 'PRI', extra \
             FROM information_schema.columns WHERE table_schema = ",
        ).sql(" ORDER BY ordinal_position"))?;
        let columns = cols
            .rows
            .into_iter()
            .map(|r| {
                let extra = r[5].clone().unwrap_or_default();
                StructColumn {
                    name: r[0].clone().unwrap_or_default(),
                    data_type: r[1].clone().unwrap_or_default(),
                    nullable: r[2].as_deref() == Some("1"),
                    // auto_increment has no default value, but it is what the column gets.
                    default: r[3].clone().or_else(|| (!extra.is_empty()).then_some(extra)),
                    pk: r[4].as_deref() == Some("1"),
                }
            })
            .collect();
        let idx = self.execute(
            &filter(
                "SELECT index_name, IF(non_unique = 0, 'UNIQUE ', ''), \
             GROUP_CONCAT(column_name ORDER BY seq_in_index SEPARATOR ', ') \
             FROM information_schema.statistics WHERE table_schema = ",
            )
            .sql(" GROUP BY index_name, non_unique ORDER BY index_name"),
        )?;
        let fks = self.execute(&filter(
            "SELECT constraint_name, CONCAT('(', GROUP_CONCAT(column_name ORDER BY ordinal_position SEPARATOR ', '), \
             ') REFERENCES ', referenced_table_name, ' (', \
             GROUP_CONCAT(referenced_column_name ORDER BY ordinal_position SEPARATOR ', '), ')') \
             FROM information_schema.key_column_usage WHERE referenced_table_name IS NOT NULL AND table_schema = ",
        ).sql(" GROUP BY constraint_name, referenced_table_name ORDER BY constraint_name"))?;
        let create = self.query(&format!(
            "SHOW CREATE TABLE {}",
            Kind::MySql.qualified(schema, table)
        ))?;
        let cell = |r: &Vec<Option<String>>, i: usize| r.get(i).cloned().flatten().unwrap_or_default();
        Ok(TableStructure {
            columns,
            indexes: idx
                .rows
                .iter()
                .map(|r| (cell(r, 0), format!("{}({})", cell(r, 1), cell(r, 2))))
                .collect(),
            foreign_keys: fks.rows.iter().map(|r| (cell(r, 0), cell(r, 1))).collect(),
            ddl: create
                .rows
                .first()
                .map(|r| format!("{};", cell(r, 1)))
                .unwrap_or_default(),
        })
    }
}

/// Takes exactly `N` cells from a row (NULL becomes an empty string).
fn cells<const N: usize>(row: Vec<Option<String>>) -> DbResult<[String; N]> {
    let len = row.len();
    row.into_iter()
        .map(Option::unwrap_or_default)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| DbError::Other(format!("esperava {N} colunas, recebi {len}")))
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
        // "Buscar valor": finds the row by part of a value, in the right column, and the filter
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
                search_table(&mut c, kind, t, "nada-disso", false)
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
        };
        c.query(risky).unwrap();
        let nasty = [r"a\", r"x\'; DROP TABLE dbl_t; --", "it's", r"\n\0", "ção 🚀"];
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
            .query("SELECT * FROM tabela_que_nao_existe")
            .unwrap_err()
            .to_string();
        assert!(err.contains("tabela_que_nao_existe"), "{kind:?}: {err}");

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
            });
            started.elapsed()
        });
        std::thread::sleep(std::time::Duration::from_millis(700));
        let act = c.activity().unwrap();
        let q = act.columns.iter().position(|c| c == "consulta").unwrap();
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
                .query("UPDATE dbl_t SET name = 'trava' WHERE id = 1")
                .unwrap();
            let mut waiter = Conn::connect(&cfg, None).unwrap();
            let blocked = std::thread::spawn(move || {
                let _ = waiter.query("UPDATE dbl_t SET name = 'espera' WHERE id = 1");
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
            slow.columns.contains(&"consulta".to_string()) && !slow.rows.is_empty(),
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
            // "Executar…": the call built from the listed arguments, with the value filled in.
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
            c.query("DROP TABLE IF EXISTS dbl_copia").unwrap();
            c.query("DROP TABLE IF EXISTS dbl_copia2").unwrap();
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
                "dbl_copia",
            ))
            .unwrap();
            assert_eq!(count(&mut c, "dbl_copia"), n, "{kind:?}: rows copied");
            c.query(&table_action(
                kind,
                schema,
                "dbl_copia",
                TableAction::Rename,
                "dbl_copia2",
            ))
            .unwrap();
            c.query(&table_action(
                kind,
                schema,
                "dbl_copia2",
                TableAction::Truncate,
                "",
            ))
            .unwrap();
            assert_eq!(count(&mut c, "dbl_copia2"), "0", "{kind:?}");
            c.query(&table_action(kind, schema, "dbl_copia2", TableAction::Drop, ""))
                .unwrap();
            assert!(c.query("SELECT 1 FROM dbl_copia2").is_err(), "{kind:?}: dropped");
        }

        // Switching where unqualified names go.
        {
            let (dbs, here, schema) = c.location().unwrap();
            assert!(dbs.contains(&here), "{kind:?}: {dbs:?} / {here}");
            match kind {
                Kind::Postgres => {
                    assert_eq!(schema, "public");
                    c.query("CREATE SCHEMA IF NOT EXISTS dbl_outro").unwrap();
                    c.query("CREATE TABLE IF NOT EXISTS dbl_outro.so_aqui (x int)")
                        .unwrap();
                    c.use_schema("dbl_outro").unwrap();
                    assert_eq!(c.location().unwrap().2, "dbl_outro");
                    c.query("SELECT * FROM so_aqui").unwrap();
                    // public still answers after it.
                    c.query("SELECT * FROM dbl_t").unwrap();
                    c.use_schema("public").unwrap();
                    assert!(c.query("SELECT * FROM so_aqui").is_err());
                    c.query("DROP SCHEMA dbl_outro CASCADE").unwrap();
                }
                Kind::MySql => {
                    c.query("CREATE DATABASE IF NOT EXISTS dbl_outro").unwrap();
                    c.query("CREATE TABLE IF NOT EXISTS dbl_outro.so_aqui (x int)")
                        .unwrap();
                    c.use_schema("dbl_outro").unwrap();
                    assert_eq!(c.location().unwrap().1, "dbl_outro");
                    c.query("SELECT * FROM so_aqui").unwrap();
                    c.use_schema(db).unwrap();
                    c.query("DROP DATABASE dbl_outro").unwrap();
                }
            }
        }

        // Server overview: every fact present, and the test table among the biggest.
        let info = c.server_info().unwrap();
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
        assert!(ix.columns.contains(&"usos".to_string()), "{kind:?}: {ix:?}");

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
        c.run_batch(&[set("1", "lote-1"), set("2", "lote-2")], false)
            .unwrap();
        let names = c.query("SELECT name FROM dbl_t ORDER BY id").unwrap();
        assert_eq!(
            names
                .rows
                .iter()
                .map(|r| r[0].clone().unwrap())
                .collect::<Vec<_>>(),
            ["lote-1", "lote-2"]
        );
        let err = c
            .run_batch(&[set("1", "nao-salvo"), set("999", "x")], false)
            .unwrap_err();
        assert!(err.to_string().contains("Nada foi salvo"), "{err}");
        let r = c.query("SELECT name FROM dbl_t WHERE id = 1").unwrap();
        assert_eq!(
            r.rows[0][0].as_deref(),
            Some("lote-1"),
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
            let joined = "UPDATE dbl_t SET name = 'mudou' FROM (SELECT 1 AS one) o WHERE dbl_t.id = o.one";
            if kind == Kind::Postgres {
                let p = c.preview(joined, false).unwrap();
                assert_eq!((p.count, p.after), (1, true));
                assert_eq!(
                    p.sample.rows[0][1].as_deref(),
                    Some("mudou"),
                    "shows the values after"
                );
                let r = c.query("SELECT name FROM dbl_t WHERE id = 1").unwrap();
                assert_ne!(r.rows[0][0].as_deref(), Some("mudou"), "and undoes it");
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
                name: "nome".into(),
                data_type: "varchar(50)".into(),
                ..ColDef::new()
            });
            d.indexes.push(IndexDef {
                existing: false,
                name: String::new(),
                columns: "nome".into(),
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
            let nome = d.columns.iter_mut().find(|c| c.name == "nome").unwrap();
            nome.name = "nome_completo".into();
            nome.nullable = false;
            nome.default = "'x'".into();
            d.indexes[0].dropped = true;
            d.columns.push(ColDef {
                name: "ativo".into(),
                data_type: "boolean".into(),
                ..ColDef::new()
            });
            c.query(&d.script())
                .unwrap_or_else(|e| panic!("{kind:?}: {e}\n{}", d.script()));
            let st = c.structure(schema, "dbl_design").unwrap();
            let names: Vec<&str> = st.columns.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(names, ["id", "nome_completo", "ativo"], "{kind:?}");
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
                std::env::temp_dir().join(format!("dblite-import-{kind:?}-{}.csv", std::process::id()));
            std::fs::write(&path, "codigo;nome\n10;dez\n11;\n").unwrap();
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
            std::fs::write(&path, "codigo;nome\n12;doze\n10;repetido\n").unwrap();
            let err = run(&mut c, &spec, false).unwrap_err().to_string();
            assert!(err.contains("Nada foi importado"), "{kind:?}: {err}");
            let r = c.query("SELECT count(*) FROM dbl_t WHERE id = 12").unwrap();
            assert_eq!(r.rows[0][0].as_deref(), Some("0"), "{kind:?}: rolled back");
            spec.mapping = vec![None, None];
            assert!(run(&mut c, &spec, false).is_err());
            let _ = std::fs::remove_file(path);
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
            at(&mut c, "SELECT * FROM dbl_nada").as_deref(),
            Some("dbl_nada"),
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
