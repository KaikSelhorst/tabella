//! Running SQL on a connection: the editor's, script files, capped reads, exports, compares.

use super::*;

/// Rows per query when exporting a whole table.
pub(super) const EXPORT_CHUNK: usize = 5000;

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

/// How many of a script file's slowest statements are kept.
pub(super) const SLOWEST_KEPT: usize = 10;

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
pub(super) fn capped_read(conn: &mut Conn, pg: bool, in_tx: bool, sql: &str) -> DbResult<QueryResult> {
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

/// Both sides of "Compare structure": the target on this connection, or on its own one
/// (through its SSH tunnel), closed when done.
pub(super) fn compare_with(
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
pub(super) fn run_user_sql(conn: &mut Conn, tx: &mut TxState, pg: bool, sql: &str) -> DbResult<QueryResult> {
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
pub(super) const TIMED_MAX_STATEMENTS: usize = 100;
pub(super) const TIMED_MAX_BYTES: usize = 200_000;

/// Runs what the editor sent. A script of several statements goes one statement at a time so each
/// one is timed; the result is the same the server would have given for the whole text (every
/// result set, the last statement's outcome), plus `timings`.
pub(super) fn run_editor_sql(
    conn: &mut Conn,
    tx: &mut TxState,
    pg: bool,
    sql: &str,
) -> DbResult<QueryResult> {
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

pub(super) fn export(conn: &mut Conn, spec: ExportSpec) -> DbResult<(PathBuf, u64)> {
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
                // Its own cap: the row limit preference is for what the grid shows.
                let chunk = conn.query_up_to(&sql, EXPORT_CHUNK)?;
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
