//! "Export .sql": tables (one, a schema's, the whole database's) as a script that recreates
//! them anywhere: the tables without foreign keys, their rows in multi-row INSERTs read in
//! chunks (never the whole table in memory), then the foreign keys and sequences, then views,
//! functions, procedures and triggers. It runs with "Run .sql file", psql or mysql.

use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::db::{Conn, DbObject, Kind, ObjKind, TableStructure};
use crate::error::{DbError, DbResult};

/// Rows read per query, and per INSERT statement written.
const CHUNK: usize = 5000;
const ROWS_PER_INSERT: usize = 500;

#[derive(Clone, Debug)]
pub struct DumpSpec {
    pub path: PathBuf,
    /// Tables to export, in order (views are left to `objects_of`).
    pub tables: Vec<(String, String)>,
    /// Schemas whose views, functions, procedures, triggers and sequences go too.
    pub objects_of: Vec<String>,
    pub structure: bool,
    pub data: bool,
    /// `DROP … IF EXISTS` before each CREATE, to replace what the target has.
    pub drop: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DumpProgress {
    /// Tables finished, of `total`.
    pub tables: usize,
    pub total: usize,
    pub rows: u64,
    pub bytes: u64,
}

/// Writes the dump. On error or cancel the partial file is removed.
pub fn dump(
    conn: &mut Conn,
    spec: &DumpSpec,
    cancel: &AtomicBool,
    report: impl Fn(DumpProgress),
) -> DbResult<DumpProgress> {
    let res = write_dump(conn, spec, cancel, &report);
    if res.is_err() {
        let _ = std::fs::remove_file(&spec.path);
    }
    res
}

struct Out {
    w: BufWriter<std::fs::File>,
    bytes: u64,
}

impl Out {
    fn line(&mut self, s: &str) -> DbResult<()> {
        self.w
            .write_all(s.as_bytes())
            .and_then(|()| self.w.write_all(b"\n"))
            .map_err(|e| DbError::Other(format!("error writing the file: {e}")))?;
        self.bytes += s.len() as u64 + 1;
        Ok(())
    }
}

fn write_dump(
    conn: &mut Conn,
    spec: &DumpSpec,
    cancel: &AtomicBool,
    report: &impl Fn(DumpProgress),
) -> DbResult<DumpProgress> {
    let k = conn.kind();
    let file = std::fs::File::create(&spec.path)
        .map_err(|e| DbError::Other(format!("couldn't create {}: {e}", spec.path.display())))?;
    let mut out = Out {
        w: BufWriter::with_capacity(1 << 20, file),
        bytes: 0,
    };
    let mut prog = Progress {
        p: DumpProgress {
            total: spec.tables.len(),
            ..Default::default()
        },
        cancel,
        report,
    };

    out.line(&format!(
        "-- Exported by tabella on {} (UTC)",
        crate::export::timestamp()
    ))?;
    match k {
        Kind::Postgres => {
            out.line("SET client_encoding = 'UTF8';")?;
            out.line("SET standard_conforming_strings = on;")?;
            // Function bodies may use tables and views created further down.
            out.line("SET check_function_bodies = false;")?;
        }
        Kind::MySql => {
            // Names aren't tied to a database: it loads into the one in use.
            out.line("-- Run it on the target database (USE database_name).")?;
            out.line("SET NAMES utf8mb4;")?;
            // Tables come in any order; their foreign keys stay in the CREATE.
            out.line("SET FOREIGN_KEY_CHECKS = 0;")?;
        }
        Kind::Sqlite => {
            // Tables come in any order and keep their foreign keys: checked at the end of a
            // transaction ("Run file"), or not at all one statement at a time (sqlite3).
            out.line("PRAGMA foreign_keys = OFF;")?;
            out.line("PRAGMA defer_foreign_keys = ON;")?;
        }
    }

    // Each table's structure once: the CREATE, then (after the data) its keys and sequences.
    let mut structures: Vec<TableStructure> = Vec::with_capacity(spec.tables.len());
    for (schema, table) in &spec.tables {
        structures.push(conn.structure(schema, table)?);
    }
    let objects: Vec<DbObject> = if spec.objects_of.is_empty() {
        Vec::new()
    } else {
        conn.objects()?
            .into_iter()
            .filter(|o| spec.objects_of.contains(&o.schema))
            .collect()
    };

    if spec.structure {
        if k == Kind::Postgres {
            let mut schemas: Vec<&String> = spec
                .tables
                .iter()
                .map(|(s, _)| s)
                .chain(&spec.objects_of)
                .collect();
            schemas.sort();
            schemas.dedup();
            out.line("")?;
            for s in schemas.into_iter().filter(|s| *s != "public") {
                out.line(&format!("CREATE SCHEMA IF NOT EXISTS {};", k.quote_ident(s)))?;
            }
            // Sequences first: column defaults call them. Those belonging to a column (serial,
            // identity) come with their table instead.
            let owned = owned_sequences(conn)?;
            for o in objects.iter().filter(|o| {
                o.kind == ObjKind::Sequence && !owned.contains(&(o.schema.clone(), o.name.clone()))
            }) {
                let src = conn.object_source(o)?;
                out.line(&src.replacen("CREATE SEQUENCE ", "CREATE SEQUENCE IF NOT EXISTS ", 1))?;
                let q = k.qualified(&o.schema, &o.name);
                let r = conn.query(&format!("SELECT last_value, is_called FROM {q}"))?;
                if let Some(row) = r.rows.first()
                    && let (Some(v), Some(called)) = (row.cell(0), row.cell(1))
                {
                    let called = if called == "t" { "true" } else { "false" };
                    out.line(&format!("SELECT setval({}, {v}, {called});", k.quote_str(&q)))?;
                }
            }
            for st in &structures {
                for (_, seq) in &st.sequences {
                    out.line(&format!("CREATE SEQUENCE IF NOT EXISTS {seq};"))?;
                }
            }
        }
        for ((schema, table), st) in spec.tables.iter().zip(&structures) {
            out.line("")?;
            if spec.drop {
                let cascade = if k == Kind::Postgres { " CASCADE" } else { "" };
                // (SQLite: no CASCADE; foreign key checks are off for the file.)
                out.line(&format!(
                    "DROP TABLE IF EXISTS {}{cascade};",
                    table_name(k, schema, table)
                ))?;
            }
            out.line(&st.ddl_without_fks)?;
            // A serial's sequence belongs to its column (dropped with it, found by
            // pg_get_serial_sequence).
            for (col, seq) in &st.sequences {
                out.line(&format!(
                    "ALTER SEQUENCE {seq} OWNED BY {}.{};",
                    k.qualified(schema, table),
                    k.quote_ident(col)
                ))?;
            }
        }
    }

    if spec.data {
        for ((schema, table), st) in spec.tables.iter().zip(&structures) {
            prog.check()?;
            out.line("")?;
            prog.p.rows += table_rows(conn, schema, table, st, &mut out, &mut prog)?;
            prog.p.tables += 1;
            prog.p.bytes = out.bytes;
        }
    } else {
        prog.p.tables = prog.p.total;
    }

    if spec.structure {
        let mut tail = Vec::new();
        for ((schema, table), st) in spec.tables.iter().zip(&structures) {
            tail.extend(st.fk_statements.iter().cloned());
            if k == Kind::Postgres && spec.data {
                // Where each column's sequence is now, so new rows go on from there.
                let q = k.qualified(schema, table);
                for name in &st.serial_columns {
                    // The column goes as its plain name (pg_get_serial_sequence takes it literally).
                    let target = format!(
                        "pg_get_serial_sequence({}, {})",
                        k.quote_str(&q),
                        k.quote_str(name)
                    );
                    let seq = conn.query(&format!("SELECT {target}"))?;
                    let Some(Some(seq)) = seq.rows.first().map(|r| r.cell(0).map(str::to_owned)) else {
                        continue;
                    };
                    let now = conn.query(&format!("SELECT last_value, is_called FROM {seq}"))?;
                    if let Some(row) = now.rows.first()
                        && let (Some(v), Some(called)) = (row.cell(0), row.cell(1))
                    {
                        let called = if called == "t" { "true" } else { "false" };
                        tail.push(format!("SELECT setval({target}, {v}, {called});"));
                    }
                }
            }
        }
        if !tail.is_empty() {
            out.line("")?;
            for l in tail {
                out.line(&l)?;
            }
        }
        // Functions before what may use them: procedures, views, triggers.
        let order = [
            ObjKind::Function,
            ObjKind::Procedure,
            ObjKind::View,
            ObjKind::Trigger,
        ];
        for kind in order {
            for o in objects.iter().filter(|o| o.kind == kind) {
                prog.check()?;
                out.line("")?;
                object_ddl(conn, k, o, spec.drop, &mut out)?;
            }
        }
    }
    if k == Kind::MySql {
        out.line("")?;
        out.line("SET FOREIGN_KEY_CHECKS = 1;")?;
    }
    out.w
        .flush()
        .map_err(|e| DbError::Other(format!("error writing the file: {e}")))?;
    prog.p.bytes = out.bytes;
    report(prog.p);
    Ok(prog.p)
}

/// Postgres sequences that belong to a column (serial, identity), as `(schema, name)`.
fn owned_sequences(conn: &mut Conn) -> DbResult<Vec<(String, String)>> {
    let r = conn.query(
        "SELECT n.nspname, c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_depend d ON d.objid = c.oid AND d.classid = 'pg_class'::regclass \
         AND d.refobjsubid > 0 AND d.deptype IN ('a', 'i') WHERE c.relkind = 'S'",
    )?;
    Ok(r.rows
        .into_iter()
        .map(|r| (r[0].clone().unwrap_or_default(), r[1].clone().unwrap_or_default()))
        .collect())
}

/// A table as the dump names it: Postgres keeps the schema; MySQL leaves the database out so
/// the file loads into whichever one is in use.
fn table_name(k: Kind, schema: &str, table: &str) -> String {
    match k {
        Kind::Postgres => k.qualified(schema, table),
        Kind::MySql | Kind::Sqlite => k.quote_ident(table),
    }
}

/// Where the dump is, how to report it, and whether to stop.
struct Progress<'a, R: Fn(DumpProgress)> {
    p: DumpProgress,
    cancel: &'a AtomicBool,
    report: &'a R,
}

impl<R: Fn(DumpProgress)> Progress<'_, R> {
    /// Stops with an error once the user cancelled.
    fn stop_if_cancelled(&self) -> DbResult<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(DbError::Other("cancelled".into()));
        }
        Ok(())
    }

    /// Between steps: honours a cancel, otherwise reports.
    fn check(&self) -> DbResult<()> {
        self.stop_if_cancelled()?;
        (self.report)(self.p);
        Ok(())
    }
}

/// The rows of one table as INSERTs; returns how many.
fn table_rows(
    conn: &mut Conn,
    schema: &str,
    table: &str,
    st: &TableStructure,
    out: &mut Out,
    prog: &mut Progress<'_, impl Fn(DumpProgress)>,
) -> DbResult<u64> {
    let k = conn.kind();
    let cols: Vec<_> = st
        .columns
        .iter()
        .filter(|c| !st.generated.contains(&c.name))
        .collect();
    if cols.is_empty() {
        return Ok(0);
    }
    // MySQL hands binary columns over as text, which would mangle them: as hex instead.
    let binary: Vec<bool> = cols
        .iter()
        .map(|c| k != Kind::Postgres && is_binary(&c.data_type))
        .collect();
    let select: Vec<String> = cols
        .iter()
        .zip(&binary)
        .map(|(c, &bin)| {
            let q = k.quote_ident(&c.name);
            if bin { format!("HEX({q})") } else { q }
        })
        .collect();
    // Ordered so chunks neither skip nor repeat rows. With a primary key each chunk starts
    // after the last key read (constant cost per chunk); without one, by every column and
    // OFFSET (slower on big tables, but there is nothing better to page by).
    let pk_at: Vec<usize> = cols
        .iter()
        .enumerate()
        .filter(|(_, c)| c.pk)
        .map(|(i, _)| i)
        .collect();
    let keyset = !pk_at.is_empty() && pk_at.iter().all(|&i| !binary[i]);
    let pk_list = pk_at
        .iter()
        .map(|&i| k.quote_ident(&cols[i].name))
        .collect::<Vec<_>>()
        .join(", ");
    let order = if keyset {
        pk_list.clone()
    } else {
        (1..=cols.len())
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut after: Option<String> = None;
    // Read from the table itself; written under the name the dump uses.
    let source = k.qualified(schema, table);
    let qualified = table_name(k, schema, table);
    let names = cols
        .iter()
        .map(|c| k.quote_ident(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    // Identity columns keep their values (and a serial's sequence is set afterwards).
    let overriding = if k == Kind::Postgres && !st.serial_columns.is_empty() {
        " OVERRIDING SYSTEM VALUE"
    } else {
        ""
    };
    let mut written = 0u64;
    let mut offset = 0usize;
    loop {
        prog.stop_if_cancelled()?;
        let page = match (&after, keyset) {
            (Some(last), true) => format!("WHERE ({pk_list}) > ({last}) ORDER BY {order} LIMIT {CHUNK}"),
            (None, true) => format!("ORDER BY {order} LIMIT {CHUNK}"),
            (_, false) => format!("ORDER BY {order} LIMIT {CHUNK} OFFSET {offset}"),
        };
        // Its own cap: the row limit preference is for what the grid shows.
        let chunk = conn.query_up_to(
            &format!("SELECT {} FROM {source} {page}", select.join(", ")),
            CHUNK,
        )?;
        if keyset && let Some(last) = chunk.rows.last() {
            let key: Vec<String> = pk_at
                .iter()
                .map(|&i| last.cell(i).map_or("NULL".into(), |v| k.quote_str(v)))
                .collect();
            after = Some(key.join(", "));
        }
        for rows in chunk.rows.chunks(ROWS_PER_INSERT) {
            out.line(&format!("INSERT INTO {qualified} ({names}){overriding} VALUES"))?;
            let n = rows.len();
            for (i, row) in rows.enumerate() {
                let vals: Vec<String> = row
                    .iter()
                    .zip(&binary)
                    .map(|(v, &bin)| match v {
                        None => "NULL".to_string(),
                        Some(v) if bin => format!("X'{v}'"),
                        Some(v) => k.quote_str(v),
                    })
                    .collect();
                let end = if i + 1 == n { ";" } else { "," };
                out.line(&format!("  ({}){end}", vals.join(", ")))?;
            }
        }
        written += chunk.rows.len() as u64;
        offset += chunk.rows.len();
        prog.p.bytes = out.bytes;
        (prog.report)(DumpProgress {
            rows: prog.p.rows + written,
            ..prog.p
        });
        if chunk.rows.len() < CHUNK {
            return Ok(written);
        }
    }
}

fn is_binary(data_type: &str) -> bool {
    let t = data_type.to_lowercase();
    t.contains("blob") || t.contains("binary") || t.starts_with("bit") || t.contains("geometry")
}

/// A view, function, procedure or trigger as a statement that creates it.
fn object_ddl(conn: &mut Conn, k: Kind, o: &DbObject, drop: bool, out: &mut Out) -> DbResult<()> {
    let src = conn.object_source(o)?;
    match k {
        Kind::Sqlite => {
            // The source has the DROP as a comment: here it's a real one, when asked for.
            let body: String = src
                .lines()
                .filter(|l| !l.starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n");
            if drop {
                let what = if o.kind == ObjKind::View {
                    "VIEW"
                } else {
                    "TRIGGER"
                };
                out.line(&format!("DROP {what} IF EXISTS {};", k.quote_ident(&o.name)))?;
            }
            out.line(&body)?;
        }
        Kind::Postgres => {
            // Views and functions come as CREATE OR REPLACE; a trigger needs room.
            if o.kind == ObjKind::Trigger && drop {
                out.line(&format!(
                    "DROP TRIGGER IF EXISTS {} ON {};",
                    k.quote_ident(&o.name),
                    k.qualified(&o.schema, &o.table)
                ))?;
            }
            let src = src.trim_end();
            out.line(src)?;
            // pg_get_functiondef leaves out the final semicolon.
            if !src.ends_with(';') {
                out.line(";")?;
            }
        }
        Kind::MySql => {
            // The DROP comment and the DEFINER (a user the target may not have) go.
            let body: String = src
                .lines()
                .filter(|l| !l.starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n");
            // Views name their tables with the database: left out, like the tables.
            let body = strip_definer(&body).replace(&format!("{}.", k.quote_ident(&o.schema)), "");
            let what = match o.kind {
                ObjKind::Function => "FUNCTION",
                ObjKind::Procedure => "PROCEDURE",
                ObjKind::Trigger => "TRIGGER",
                ObjKind::View | ObjKind::Sequence => "VIEW",
            };
            if drop && o.kind != ObjKind::View {
                out.line(&format!("DROP {what} IF EXISTS {};", k.quote_ident(&o.name)))?;
            }
            if o.kind == ObjKind::View {
                out.line(&body)?;
            } else {
                // Routine bodies have their own semicolons.
                out.line("DELIMITER ;;")?;
                out.line(&format!("{} ;;", body.trim_end().trim_end_matches(';')))?;
                out.line("DELIMITER ;")?;
            }
        }
    }
    Ok(())
}

/// `CREATE DEFINER=`root`@`%` FUNCTION …` → `CREATE FUNCTION …`.
fn strip_definer(sql: &str) -> String {
    let Some(at) = sql.find("DEFINER=`") else {
        return sql.to_string();
    };
    // DEFINER=`user`@`host` and the space after it.
    let rest = &sql[at..];
    let mut ticks = 0;
    let end = rest
        .char_indices()
        .find(|&(_, c)| {
            if c == '`' {
                ticks += 1;
            }
            ticks == 4
        })
        .map_or(rest.len(), |(i, _)| i + 1);
    format!("{}{}", &sql[..at], rest[end..].trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definers_are_left_out() {
        assert_eq!(
            strip_definer("CREATE DEFINER=`root`@`%` FUNCTION f() RETURNS int RETURN 1"),
            "CREATE FUNCTION f() RETURNS int RETURN 1"
        );
        assert_eq!(
            strip_definer(
                "CREATE ALGORITHM=UNDEFINED DEFINER=`app`@`10.0.%` SQL SECURITY DEFINER VIEW v AS SELECT 1"
            ),
            "CREATE ALGORITHM=UNDEFINED SQL SECURITY DEFINER VIEW v AS SELECT 1"
        );
        assert_eq!(
            strip_definer("CREATE VIEW v AS SELECT 1"),
            "CREATE VIEW v AS SELECT 1"
        );
        assert!(is_binary("varbinary(16)") && is_binary("LONGBLOB") && !is_binary("varchar(10)"));
    }

    /// Needs `docker compose --profile test up -d --wait`. Dumps a schema with the awkward
    /// cases, drops it, runs the file back and checks everything came back.
    fn round_trip(kind: Kind, port: u16) {
        use crate::db::ConnConfig;
        let mut cfg = ConnConfig::new(kind);
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        cfg.password = "pw".into();
        cfg.database = if kind == Kind::Postgres {
            "postgres"
        } else {
            "test"
        }
        .into();
        let mut c = Conn::connect(&cfg, None).expect("connect");
        let q = |c: &mut Conn, sql: &str| c.query(sql).unwrap_or_else(|e| panic!("{kind:?}: {sql}: {e}"));
        let one = |c: &mut Conn, sql: &str| q(c, sql).rows.cell(0, 0).map(str::to_owned).unwrap_or_default();
        let schema = "dbl_dump";
        let setup = match kind {
            Kind::Postgres => vec![
                "DROP SCHEMA IF EXISTS dbl_dump CASCADE",
                "CREATE SCHEMA dbl_dump",
                "CREATE TABLE dbl_dump.customer (id serial PRIMARY KEY, name text NOT NULL, photo bytea, \
                 active boolean DEFAULT true, name_upper text GENERATED ALWAYS AS (upper(name)) STORED)",
                "CREATE TABLE dbl_dump.purchase (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                 customer_id int NOT NULL REFERENCES dbl_dump.customer (id), note text)",
                "CREATE SEQUENCE dbl_dump.note_seq START 100",
                "SELECT nextval('dbl_dump.note_seq') FROM generate_series(1, 5)",
                "INSERT INTO dbl_dump.customer (name, photo, active) SELECT 'c' || g || E' \\\\ ''q''', \
                 CASE WHEN g % 3 = 0 THEN '\\x00ff10'::bytea END, g % 2 = 0 FROM generate_series(1, 6001) g",
                "DELETE FROM dbl_dump.customer WHERE id > 6000",
                "INSERT INTO dbl_dump.purchase (customer_id, note) SELECT 1 + g % 100, \
                 CASE WHEN g % 5 = 0 THEN E'line\\nbroken; com point' END FROM generate_series(1, 700) g",
                "CREATE FUNCTION dbl_dump.twice(x int) RETURNS int LANGUAGE sql AS $$ SELECT x * 2 $$",
                "CREATE VIEW dbl_dump.summary AS SELECT customer_id, count(*) AS n FROM dbl_dump.purchase GROUP BY 1",
                "CREATE FUNCTION dbl_dump.touch() RETURNS trigger LANGUAGE plpgsql AS \
                 $$ BEGIN NEW.note := coalesce(NEW.note, 'auto'); RETURN NEW; END $$",
                "CREATE TRIGGER purchase_touch BEFORE INSERT ON dbl_dump.purchase FOR EACH ROW EXECUTE FUNCTION dbl_dump.touch()",
            ],
            Kind::MySql => vec![
                "DROP DATABASE IF EXISTS dbl_dump",
                "CREATE DATABASE dbl_dump",
                "CREATE TABLE dbl_dump.customer (id int AUTO_INCREMENT PRIMARY KEY, name varchar(80) NOT NULL, photo blob, \
                 active boolean DEFAULT true, name_upper varchar(80) GENERATED ALWAYS AS (upper(name)) STORED)",
                "CREATE TABLE dbl_dump.purchase (id int AUTO_INCREMENT PRIMARY KEY, customer_id int NOT NULL, note text, \
                 FOREIGN KEY (customer_id) REFERENCES dbl_dump.customer (id))",
                "SET SESSION cte_max_recursion_depth = 10000",
                "INSERT INTO dbl_dump.customer (name, photo, active) \
                 WITH RECURSIVE s (g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM s WHERE g < 6000) \
                 SELECT CONCAT('c', g, ' \\\\ ''q'''), IF(g % 3 = 0, UNHEX('00FF10'), NULL), g % 2 = 0 FROM s",
                "INSERT INTO dbl_dump.purchase (customer_id, note) \
                 WITH RECURSIVE s (g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM s WHERE g < 700) \
                 SELECT 1 + g % 100, IF(g % 5 = 0, 'line\\nbroken; com point', NULL) FROM s",
                "CREATE FUNCTION dbl_dump.twice(x int) RETURNS int DETERMINISTIC RETURN x * 2",
                "CREATE VIEW dbl_dump.summary AS SELECT customer_id, count(*) AS n FROM dbl_dump.purchase GROUP BY customer_id",
                "CREATE TRIGGER dbl_dump.purchase_touch BEFORE INSERT ON dbl_dump.purchase \
                 FOR EACH ROW BEGIN SET NEW.note = coalesce(NEW.note, 'auto'); END",
            ],
            Kind::Sqlite => unreachable!("server test"),
        };
        for sql in setup {
            q(&mut c, sql);
        }
        let fingerprint = |c: &mut Conn| {
            let md5 = |col: &str| match kind {
                Kind::Postgres => format!("md5(string_agg({col}, '|' ORDER BY id))"),
                Kind::MySql => format!("md5(GROUP_CONCAT({col} ORDER BY id SEPARATOR '|'))"),
                Kind::Sqlite => unreachable!("server test"),
            };
            let photo = match kind {
                Kind::Postgres => "coalesce(encode(photo, 'hex'), '-')",
                Kind::MySql => "coalesce(hex(photo), '-')",
                Kind::Sqlite => unreachable!("server test"),
            };
            let customer = md5(&format!("concat(id, name, {photo}, active, name_upper)"));
            vec![
                one(c, &format!("SELECT {customer} FROM dbl_dump.customer")),
                one(
                    c,
                    &format!(
                        "SELECT {} FROM dbl_dump.purchase",
                        md5("concat(id, customer_id, coalesce(note, '-'))")
                    ),
                ),
                one(c, "SELECT count(*) FROM dbl_dump.summary"),
                one(c, "SELECT dbl_dump.twice(21)"),
            ]
        };
        // A row added before the dump: the copy must number the next one right after it.
        q(&mut c, "INSERT INTO dbl_dump.customer (name) VALUES ('new')");
        q(&mut c, "INSERT INTO dbl_dump.purchase (customer_id) VALUES (1)");
        let last = |c: &mut Conn, t: &str| -> i64 {
            one(c, &format!("SELECT max(id) FROM dbl_dump.{t}"))
                .parse()
                .unwrap()
        };
        let (last_customer, last_purchase) = (last(&mut c, "customer"), last(&mut c, "purchase"));
        let before = fingerprint(&mut c);
        assert_eq!(before[3], "42");

        let tables: Vec<(String, String)> = c
            .tables()
            .unwrap()
            .into_iter()
            .filter(|t| t.schema == schema && !t.is_view)
            .map(|t| (t.schema, t.name))
            .collect();
        assert_eq!(tables.len(), 2, "{kind:?}");
        let path = std::env::temp_dir().join(format!("tabella-dump-{kind:?}-{}.sql", std::process::id()));
        let spec = DumpSpec {
            path: path.clone(),
            tables,
            objects_of: vec![schema.into()],
            structure: true,
            data: true,
            drop: true,
        };
        let p =
            dump(&mut c, &spec, &AtomicBool::new(false), |_| {}).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        assert_eq!((p.tables, p.rows), (2, 6702), "{kind:?}");

        // Gone, then back from the file.
        match kind {
            Kind::Postgres => {
                q(&mut c, "DROP SCHEMA dbl_dump CASCADE");
            }
            Kind::MySql => {
                q(&mut c, "DROP DATABASE dbl_dump");
                q(&mut c, "CREATE DATABASE dbl_dump");
                q(&mut c, "USE dbl_dump");
            }
            Kind::Sqlite => unreachable!("server test"),
        }
        let mut tx = crate::worker::TxState::default();
        let pg = kind == Kind::Postgres;
        let (_, err, _) =
            crate::worker::run_script(&mut c, &mut tx, pg, &path, pg, &AtomicBool::new(false), |_| {});
        if let Some(e) = err {
            panic!("{kind:?}: line {}: {}\n{}", e.line, e.message, e.statement);
        }
        assert_eq!(fingerprint(&mut c), before, "{kind:?}");
        // Numbering goes on from where it was; the trigger is back.
        q(&mut c, "INSERT INTO dbl_dump.customer (name) VALUES ('one more')");
        assert_eq!(last(&mut c, "customer"), last_customer + 1, "{kind:?}");
        q(&mut c, "INSERT INTO dbl_dump.purchase (customer_id) VALUES (1)");
        assert_eq!(last(&mut c, "purchase"), last_purchase + 1, "{kind:?}");
        let obs = one(
            &mut c,
            &format!(
                "SELECT note FROM dbl_dump.purchase WHERE id = {}",
                last_purchase + 1
            ),
        );
        assert_eq!(obs, "auto", "{kind:?}");
        if pg {
            assert_eq!(one(&mut c, "SELECT nextval('dbl_dump.note_seq')"), "105");
            // No stray copies of the serial/identity sequences.
            let seqs = "SELECT string_agg(sequencename, ',' ORDER BY sequencename) FROM pg_sequences WHERE schemaname = 'dbl_dump'";
            assert_eq!(one(&mut c, seqs), "customer_id_seq,note_seq,purchase_id_seq");
            q(&mut c, "DROP SCHEMA dbl_dump CASCADE");
        } else {
            q(&mut c, "DROP DATABASE dbl_dump");
        }
        if std::env::var("TABELLA_KEEP_DUMP").is_err() {
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn round_trip_postgres() {
        round_trip(Kind::Postgres, 55432);
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn round_trip_mysql() {
        round_trip(Kind::MySql, 53306);
    }

    /// A real database, by hand: `TABELLA_DUMP_CHECK=host:port:user:password:database:schema`
    /// (Postgres). Exports the schema, restores it into a new database `dbl_restore` on the
    /// same server, compares row counts and a checksum per table, then drops `dbl_restore`.
    #[test]
    #[ignore = "by hand against a real database: set TABELLA_DUMP_CHECK"]
    fn round_trip_real_database() {
        use crate::db::ConnConfig;
        let Ok(spec) = std::env::var("TABELLA_DUMP_CHECK") else {
            return;
        };
        let [host, port, user, password, database, schema] = spec.split(':').collect::<Vec<_>>()[..] else {
            panic!("host:port:user:password:database:schema")
        };
        let cfg = ConnConfig {
            host: host.into(),
            port: port.parse().unwrap(),
            user: user.into(),
            password: password.into(),
            database: database.into(),
            ..ConnConfig::new(Kind::Postgres)
        };
        let mut src = Conn::connect(&cfg, None).expect("connect");
        let tables: Vec<(String, String)> = src
            .tables()
            .unwrap()
            .into_iter()
            .filter(|t| t.schema == schema && !t.is_view)
            .map(|t| (t.schema, t.name))
            .collect();
        let path = std::env::temp_dir().join("tabella-dump-real.sql");
        let started = std::time::Instant::now();
        let spec = DumpSpec {
            path: path.clone(),
            tables: tables.clone(),
            objects_of: vec![schema.into()],
            structure: true,
            data: true,
            drop: false,
        };
        let p = dump(&mut src, &spec, &AtomicBool::new(false), |_| {}).unwrap();
        eprintln!(
            "dump: {} rows, {} MB, {:?}",
            p.rows,
            p.bytes >> 20,
            started.elapsed()
        );

        src.query("DROP DATABASE IF EXISTS dbl_restore").unwrap();
        src.query("CREATE DATABASE dbl_restore").unwrap();
        let mut dst = Conn::connect(
            &ConnConfig {
                database: "dbl_restore".into(),
                ..cfg
            },
            None,
        )
        .unwrap();
        let started = std::time::Instant::now();
        let mut tx = crate::worker::TxState::default();
        let (_, err, _) = crate::worker::run_script(
            &mut dst,
            &mut tx,
            true,
            &path,
            true,
            &AtomicBool::new(false),
            |_| {},
        );
        if let Some(e) = err {
            panic!("line {}: {}\n{}", e.line, e.message, e.statement);
        }
        eprintln!("restore: {:?}", started.elapsed());
        for (sc, t) in &tables {
            let q = Kind::Postgres.qualified(sc, t);
            let check =
                format!("SELECT count(*), md5(coalesce(sum(hashtext(x::text))::text, '')) FROM {q} x");
            let a = src.query(&check).unwrap().rows;
            let b = dst.query(&check).unwrap().rows;
            assert_eq!(a, b, "{q}");
            eprintln!("{q}: {:?} ok", a.cell(0, 0));
        }
        drop(dst);
        src.query("DROP DATABASE dbl_restore").unwrap();
        let _ = std::fs::remove_file(&path);
    }

    /// SQLite, no server needed: dump the sample file, load it into a new one with
    /// "Run file", and find the same rows, blob, view, trigger and numbering.
    /// A low row limit (a preference for the grid) doesn't cut the data short.
    #[test]
    fn whole_table_past_the_row_limit() {
        let cfg = crate::sqlite::tests::sample_db("dump-limit");
        let mut c = Conn::connect(&cfg, None).unwrap();
        c.query(
            "CREATE TABLE big (id INTEGER PRIMARY KEY, v TEXT);
             WITH RECURSIVE s (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 12000)
             INSERT INTO big SELECT n, 'v' || n FROM s",
        )
        .unwrap();
        let path = std::env::temp_dir().join(format!("tabella-dump-limit-{}.sql", std::process::id()));
        let spec = DumpSpec {
            path: path.clone(),
            tables: vec![("main".into(), "big".into())],
            objects_of: Vec::new(),
            structure: false,
            data: true,
            drop: false,
        };
        let p =
            crate::db::with_max_rows(1000, || dump(&mut c, &spec, &AtomicBool::new(false), |_| {})).unwrap();
        assert_eq!(p.rows, 12_000);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&cfg.database);
    }

    #[test]
    fn round_trip_sqlite() {
        let cfg = crate::sqlite::tests::sample_db("dump-src");
        let mut c = Conn::connect(&cfg, None).unwrap();
        let one = |c: &mut Conn, sql: &str| {
            c.query(sql)
                .unwrap()
                .rows
                .cell(0, 0)
                .map(str::to_owned)
                .unwrap_or_default()
        };
        let fingerprint = |c: &mut Conn| {
            vec![
                one(
                    c,
                    "SELECT group_concat(id || '|' || name || '|' || coalesce(email, '-') || '|' || active, ';') FROM (SELECT * FROM customer ORDER BY id)",
                ),
                one(
                    c,
                    "SELECT group_concat(id || '|' || customer_id || '|' || total || '|' || coalesce(hex(photo), '-') || '|' || note, ';') FROM (SELECT * FROM purchase ORDER BY id)",
                ),
                one(c, "SELECT group_concat(customer_id || ':' || n) FROM summary"),
            ]
        };
        let before = fingerprint(&mut c);
        let tables: Vec<(String, String)> = c
            .tables()
            .unwrap()
            .into_iter()
            .filter(|t| !t.is_view)
            .map(|t| (t.schema, t.name))
            .collect();
        let path = std::env::temp_dir().join(format!("tabella-dump-sqlite-{}.sql", std::process::id()));
        let spec = DumpSpec {
            path: path.clone(),
            tables,
            objects_of: vec!["main".into()],
            structure: true,
            data: true,
            drop: true,
        };
        let p = dump(&mut c, &spec, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!((p.tables, p.rows), (2, 5));

        let target = crate::db::ConnConfig {
            database: format!("{}-copy.db", cfg.database),
            ..cfg.clone()
        };
        let _ = std::fs::remove_file(&target.database);
        crate::sqlite::create(&target.database).unwrap();
        let mut d = Conn::connect(&target, None).unwrap();
        let mut tx = crate::worker::TxState::default();
        let (_, err, _) = crate::worker::run_script(
            &mut d,
            &mut tx,
            false,
            &path,
            true,
            &AtomicBool::new(false),
            |_| {},
        );
        if let Some(e) = err {
            panic!("line {}: {}\n{}", e.line, e.message, e.statement);
        }
        assert_eq!(fingerprint(&mut d), before);
        // The trigger came back; numbering goes on after the copied ids.
        d.query("INSERT INTO purchase (customer_id, total) VALUES (2, 1)")
            .unwrap();
        assert_eq!(
            one(
                &mut d,
                "SELECT id || ':' || note FROM purchase ORDER BY id DESC LIMIT 1"
            ),
            "4:auto"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&cfg.database);
        let _ = std::fs::remove_file(&target.database);
    }
}
