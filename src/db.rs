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
}

#[derive(Clone, Debug)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub pk: bool,
    pub nullable: bool,
}

fn my_opts(cfg: &ConnConfig) -> mysql::OptsBuilder {
    let mut b = mysql::OptsBuilder::new()
        .ip_or_hostname(Some(cfg.host.clone()))
        .tcp_port(cfg.port)
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

pub enum Conn {
    Pg(Box<postgres::Client>),
    My(mysql::Conn),
}

impl Conn {
    pub fn connect(cfg: &ConnConfig) -> DbResult<Self> {
        let mut conn = Self::open(cfg)?;
        if cfg.read_only {
            // Enforced by the server, so the SQL editor can't write either.
            conn.query(match cfg.kind {
                Kind::Postgres => "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
                Kind::MySql => "SET SESSION TRANSACTION READ ONLY",
            })?;
        }
        Ok(conn)
    }

    fn open(cfg: &ConnConfig) -> DbResult<Self> {
        match cfg.kind {
            Kind::Postgres => {
                let mut c = postgres::Config::new();
                c.host(&cfg.host)
                    .port(cfg.port)
                    .user(&cfg.user)
                    .password(&cfg.password)
                    .connect_timeout(Duration::from_secs(10));
                if !cfg.database.is_empty() {
                    c.dbname(&cfg.database);
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
            Kind::MySql => Ok(Conn::My(mysql::Conn::new(my_opts(cfg))?)),
        }
    }

    /// A handle that can stop the statement this connection is running, from another thread.
    pub fn canceller(&self, cfg: &ConnConfig) -> Canceller {
        match self {
            Conn::Pg(c) => Canceller::Pg(c.cancel_token(), cfg.ssl_insecure || !cfg.ssl),
            Conn::My(c) => Canceller::My(Box::new(my_opts(cfg)), c.connection_id()),
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
                "SELECT t.table_schema, t.table_name, t.table_type, c.reltuples::bigint \
                 FROM information_schema.tables t \
                 LEFT JOIN pg_catalog.pg_namespace n ON n.nspname = t.table_schema \
                 LEFT JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = t.table_name \
                 WHERE t.table_schema NOT IN ('pg_catalog', 'information_schema') \
                 ORDER BY 1, 2"
            }
            Conn::My(_) => {
                "SELECT table_schema, table_name, table_type, table_rows \
                 FROM information_schema.tables \
                 WHERE table_schema NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') \
                 ORDER BY 1, 2"
            }
        };
        let r = self.query(sql)?;
        r.rows
            .into_iter()
            .map(|row| {
                let [schema, name, ty, rows] = cells(row)?;
                let is_view = ty.contains("VIEW");
                Ok(TableRef {
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
                 c.is_nullable \
                 FROM information_schema.columns c WHERE c.table_schema = ",
            ),
            Kind::MySql => Stmt::new(
                "SELECT column_name, column_type, column_key = 'PRI', is_nullable \
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
                let [name, data_type, pk, nullable] = cells(row)?;
                Ok(ColumnInfo {
                    name,
                    data_type,
                    pk: matches!(pk.as_str(), "t" | "1" | "true"),
                    nullable: nullable == "YES",
                })
            })
            .collect()
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
        let mut c = Conn::connect(&cfg).expect("connect");
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
        let canceller = c.canceller(&cfg);
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

        // Read-only connections are enforced by the server.
        let ro = ConnConfig {
            read_only: true,
            ..cfg.clone()
        };
        let mut r = Conn::connect(&ro).expect("read-only connect");
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
        assert!(Conn::connect(&cfg).is_err(), "self-signed cert must be rejected");
        cfg.ssl_insecure = true;
        let mut c = Conn::connect(&cfg).expect("insecure tls connect");
        let r = c
            .query("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("t"));

        // With TLS not required, a TLS-capable server is still used encrypted (sslmode=prefer).
        cfg.ssl = false;
        let mut c = Conn::connect(&cfg).expect("prefer connect");
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
        let mut c = Conn::connect(&cfg).expect("insecure tls connect");
        let r = c.query("SHOW STATUS LIKE 'Ssl_cipher'").unwrap();
        assert!(!r.rows[0][1].as_deref().unwrap_or("").is_empty());
    }
}
