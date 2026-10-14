use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use crate::db::{Canceller, ColumnInfo, Conn, ConnConfig, QueryResult, TableRef};
use crate::error::DbResult;
use crate::sql::Stmt;

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
        let mut conn = match Conn::connect(&cfg) {
            Ok(c) => c,
            Err(e) => return send(Reply::Connected(Err(e))),
        };
        let canceller = Arc::new(conn.canceller(&cfg));
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
            });
        }
    });
    (job_tx, reply_rx)
}
