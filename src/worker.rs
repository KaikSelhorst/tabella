use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use std::path::PathBuf;

use crate::db::{Canceller, ColumnInfo, Conn, ConnConfig, QueryResult, TableRef, TableStructure};
use crate::error::{DbError, DbResult};
use crate::export::{Format, Writer};
use crate::sql::{PageQuery, Stmt, select_range};
use crate::tunnel::Tunnel;

pub enum Job {
    Query {
        tab: u64,
        sql: String,
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
    Structure {
        tab: u64,
        schema: String,
        table: String,
    },
    Export {
        tab: u64,
        spec: ExportSpec,
    },
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
        order: Option<(String, bool)>,
        pk: Vec<String>,
    },
}

pub struct Connected {
    pub tables: Vec<TableRef>,
    pub canceller: Arc<Canceller>,
}

pub enum Reply {
    Connected(DbResult<Connected>),
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
    },
    Count {
        tab: u64,
        res: DbResult<i64>,
    },
    /// `(table, column)` pairs for autocomplete; sent after connecting and on table reloads.
    AllColumns(DbResult<Vec<(String, String)>>),
    Structure {
        tab: u64,
        res: DbResult<TableStructure>,
    },
    /// File written and how many rows went into it.
    Exported {
        tab: u64,
        res: DbResult<(PathBuf, u64)>,
    },
}

/// Spawns a thread that owns the connection. Dropping the returned sender closes it once the
/// current statement (if any) finishes; use the `Canceller` to stop that statement early.
pub fn spawn(cfg: ConnConfig, ctx: eframe::egui::Context) -> (Sender<Job>, Receiver<Reply>) {
    let (job_tx, job_rx) = channel::<Job>();
    let (reply_tx, reply_rx) = channel::<Reply>();
    thread::spawn(move || {
        let send = |r: Reply| {
            let _ = reply_tx.send(r);
            ctx.request_repaint();
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
        let canceller = Arc::new(conn.canceller(&cfg, via));
        send(Reply::Connected(
            conn.tables().map(|tables| Connected { tables, canceller }),
        ));
        send(Reply::AllColumns(conn.all_columns()));
        while let Ok(job) = job_rx.recv() {
            send(match job {
                Job::Query { tab, sql } => Reply::Rows {
                    tab,
                    res: Box::new(conn.query(&sql)),
                    notice: None,
                },
                Job::Exec {
                    tab,
                    stmt,
                    refresh,
                    expect_one,
                } => {
                    let mut notice = None;
                    let res = conn.execute(&stmt).and_then(|done| {
                        if expect_one && done.affected == Some(0) {
                            notice = Some(
                                "Nenhuma linha foi alterada: ela mudou ou foi excluída em outra sessão. \
                                 A página foi recarregada."
                                    .to_string(),
                            );
                        }
                        conn.query(&refresh)
                    });
                    Reply::Rows {
                        tab,
                        res: Box::new(res),
                        notice,
                    }
                }
                Job::Columns { tab, schema, table } => Reply::Columns {
                    tab,
                    res: conn.columns(&schema, &table),
                },
                Job::Count { tab, sql } => Reply::Count {
                    tab,
                    res: conn.count(&sql),
                },
                Job::Tables => {
                    let tables = conn.tables();
                    send(Reply::AllColumns(conn.all_columns()));
                    Reply::Tables(tables)
                }
                Job::Structure { tab, schema, table } => Reply::Structure {
                    tab,
                    res: conn.structure(&schema, &table),
                },
                Job::Export { tab, spec } => Reply::Exported {
                    tab,
                    res: export(&mut conn, spec),
                },
            });
        }
    });
    (job_tx, reply_rx)
}

fn export(conn: &mut Conn, spec: ExportSpec) -> DbResult<(PathBuf, u64)> {
    let io = |e: std::io::Error| DbError::Other(format!("erro ao gravar {}: {e}", spec.path.display()));
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
                order: order.as_ref().map(|(c, asc)| (c.as_str(), *asc)),
                pk: &pk,
                page: 0,
            };
            // Ordered by the primary key, so chunks neither skip nor repeat rows.
            let mut writer: Option<Writer> = None;
            let mut offset = 0;
            loop {
                let chunk = conn.query(&select_range(kind, &q, EXPORT_CHUNK, offset))?;
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
        };
        c.query(fill).unwrap();
        let path = std::env::temp_dir().join(format!("dblite-export-{kind:?}-{}.csv", std::process::id()));
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
                order: None,
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
        assert_eq!(r.rows[0][0].as_deref(), Some("it's"));
        c.query("DROP TABLE dbl_export").unwrap();
    }

    #[test]
    #[ignore]
    fn export_postgres() {
        export_whole_table(Kind::Postgres, 55432, "postgres");
    }

    #[test]
    #[ignore]
    fn export_mysql() {
        export_whole_table(Kind::MySql, 53306, "test");
    }
}
