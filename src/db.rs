use std::time::{Duration, Instant};

use mysql::Value;
use mysql::prelude::Queryable;
use postgres::SimpleQueryMessage;
use serde::{Deserialize, Serialize};

use crate::error::{DbError, DbResult};
use crate::sql::Stmt;
use crate::tls;

/// Rows kept in memory per result; protects against `SELECT *` on huge tables.
const MAX_ROWS: usize = 5000;

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
            ssl: false,
            ssl_insecure: false,
            read_only: false,
            color: None,
            timeout_secs: 0,
            ssh: None,
        }
    }
}

#[derive(Default, Clone, Debug)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub affected: Option<u64>,
    pub truncated: bool,
    pub elapsed: Duration,
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

    /// Runs one or more statements; returns the last result set (capped at `MAX_ROWS`).
    pub fn query(&mut self, sql: &str) -> DbResult<QueryResult> {
        self.query_limit(sql, MAX_ROWS)
    }

    /// Runs a generated statement (see `sql::Stmt`): bound parameters on MySQL, escaped literals
    /// on Postgres.
    pub fn execute(&mut self, stmt: &Stmt) -> DbResult<QueryResult> {
        let start = Instant::now();
        let mut out = match self {
            Conn::Pg(c) => pg_query(c, &stmt.render(Kind::Postgres), MAX_ROWS)?,
            Conn::My(c) => {
                let (sql, values) = stmt.with_placeholders();
                my_exec(c, &sql, values, MAX_ROWS)?
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
    pub fn all_columns(&mut self) -> DbResult<Vec<(String, String)>> {
        let system = match self {
            Conn::Pg(_) => "'pg_catalog', 'information_schema'",
            Conn::My(_) => "'information_schema', 'mysql', 'performance_schema', 'sys'",
        };
        let sql = format!(
            "SELECT table_name, column_name FROM information_schema.columns \
             WHERE table_schema NOT IN ({system}) ORDER BY 1, 2"
        );
        let r = self.query_limit(&sql, 100_000)?;
        r.rows
            .into_iter()
            .map(|row| {
                let [t, c] = cells(row)?;
                Ok((t, c))
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

    /// Stops another session's statement (`terminate` = close that whole connection).
    pub fn kill(&mut self, id: i64, terminate: bool) -> DbResult<()> {
        let sql = match (self.kind(), terminate) {
            (Kind::Postgres, false) => format!("SELECT pg_cancel_backend({id})"),
            (Kind::Postgres, true) => format!("SELECT pg_terminate_backend({id})"),
            (Kind::MySql, false) => format!("KILL QUERY {id}"),
            (Kind::MySql, true) => format!("KILL {id}"),
        };
        self.query(&sql).map(|_| ())
    }

    /// Runs row edits as one transaction. Every statement must touch exactly one row; if one
    /// doesn't (the row changed or vanished in another session) or fails, nothing is kept.
    pub fn run_batch(&mut self, stmts: &[Stmt]) -> DbResult<()> {
        self.query(match self.kind() {
            Kind::Postgres => "BEGIN",
            Kind::MySql => "START TRANSACTION",
        })?;
        for (i, stmt) in stmts.iter().enumerate() {
            let problem = match self.execute(stmt) {
                Ok(r) if r.affected == Some(1) => continue,
                Ok(_) => DbError::Other(format!(
                    "A alteração {} não encontrou a linha (ela mudou ou foi excluída em outra sessão). Nada foi salvo.",
                    i + 1
                )),
                Err(e) => DbError::Other(format!("Alteração {}: {e}. Nada foi salvo.", i + 1)),
            };
            let _ = self.query("ROLLBACK");
            return Err(problem);
        }
        self.query("COMMIT")?;
        Ok(())
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
        assert!(cols.contains(&("dbl_t".to_string(), "name".to_string())));
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
        c.query("DROP TABLE dbl_child").unwrap();

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
        assert!(
            canceller_target.join().unwrap() < std::time::Duration::from_secs(5),
            "{kind:?}: kill"
        );

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
        c.run_batch(&[set("1", "lote-1"), set("2", "lote-2")]).unwrap();
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
            .run_batch(&[set("1", "nao-salvo"), set("999", "x")])
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
