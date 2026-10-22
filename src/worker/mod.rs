//! A session's connections, each on its own thread, and the dispatcher between them and the
//! app: the app sends `Job`s and gets `Reply`s (tagged with the job's number) without ever
//! waiting on the database.
//!
//! - `job`: what can be asked, and which connection may run it
//! - `reply`: what comes back
//! - `run`: one connection's loop (`Worker`), reconnecting when it drops
//! - `statements`: running SQL (editor, scripts, exports, compares)
//! - `cancel`: stopping a tab's statement and its jobs still in line

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::search::{SearchHit, SearchTarget};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use std::path::PathBuf;

use crate::db::{
    ActivityView, Canceller, ColumnInfo, Conn, ConnConfig, DbObject, FkEdge, IncomingFk, PreviewData,
    QueryResult, SchemaColumn, ServerInfo, TableRef, TableStructure,
};
use crate::error::{DbError, DbResult};
use crate::export::{Format, Writer};
use crate::import::ImportSpec;
use crate::plan::Plan;
use crate::sql::{self, PageQuery, Stmt, select_range};
use crate::tunnel::{Link, Tunnel};

mod cancel;
mod job;
mod reply;
mod run;
mod statements;
#[cfg(test)]
pub(super) mod tests;

pub use cancel::{Cancels, Running};
pub use job::Job;
pub use reply::{Reply, ScriptError, ScriptProgress, TxState};
#[cfg(test)]
pub(crate) use statements::run_script;
pub use statements::{ExportSource, ExportSpec};

use run::*;
use statements::*;

/// Called from other threads when there is something new to show.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A job and when the dispatcher took it, to tell it from a cancel that came after.
struct Queued {
    job: Job,
    taken: Instant,
    /// Its place in the order the app sent jobs (0: not sent by the app).
    seq: u64,
}

impl Queued {
    fn now(job: Job) -> Self {
        Queued {
            job,
            taken: Instant::now(),
            seq: 0,
        }
    }
}

/// A reply and the `seq` of the job it answers (0 for the connection's own news). A tab's jobs
/// may run on either of the session's connections, so their replies can arrive out of order;
/// this tells a late one from the newer one already shown.
pub struct Tagged {
    pub seq: u64,
    pub reply: Reply,
}

impl From<Reply> for Tagged {
    fn from(reply: Reply) -> Self {
        Tagged { seq: 0, reply }
    }
}

pub struct Connected {
    pub tables: Vec<TableRef>,
}

/// A session's worker: where jobs go, where replies come from, and what each of its
/// connections is running (for cancelling).
pub struct Spawned {
    pub jobs: Sender<Job>,
    pub replies: Receiver<Tagged>,
    pub running: Vec<Arc<Running>>,
    pub cancels: Arc<Cancels>,
    /// The SSH tunnel its connections go through, if any.
    pub link: Option<Arc<Link>>,
}

/// "Test connection": connects (through the SSH tunnel, if any) on its own thread and says
/// which server answered and how fast, without touching the open session.
pub fn test_connection(cfg: ConnConfig, wake: Waker) -> Receiver<Result<String, String>> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let started = std::time::Instant::now();
        let res = crate::crash::guard(|| {
            let tunnel = match &cfg.ssh {
                Some(ssh) => {
                    Some(Tunnel::open(ssh, &cfg.host, cfg.port).map_err(|e| format!("SSH tunnel: {e}"))?)
                }
                None => None,
            };
            let mut conn =
                Conn::connect(&cfg, tunnel.as_ref().map(|t| t.local_port)).map_err(|e| e.explained())?;
            let ms = started.elapsed().as_millis();
            let version = conn
                .query(if cfg.kind == crate::db::Kind::Sqlite {
                    "SELECT sqlite_version()"
                } else {
                    "SELECT version()"
                })
                .ok()
                .and_then(|r| r.rows.into_iter().next())
                .and_then(|r| r.into_iter().next().flatten())
                .map(|v| server_name(cfg.kind, &v))
                .unwrap_or_default();
            Ok(format!("Connected in {ms} ms · {version}"))
        })
        .unwrap_or_else(Err);
        let _ = tx.send(res);
        wake();
    });
    rx
}

/// "PostgreSQL 16.4 (Debian …) on x86_64…" → "PostgreSQL 16.4"; MySQL's "8.0.39" → "MySQL 8.0.39".
fn server_name(kind: crate::db::Kind, version: &str) -> String {
    match kind {
        crate::db::Kind::Postgres => version.split_whitespace().take(2).collect::<Vec<_>>().join(" "),
        crate::db::Kind::MySql => format!("MySQL {}", version.split('-').next().unwrap_or(version)),
        crate::db::Kind::Sqlite => format!("SQLite {version}"),
    }
}

/// Postgres opens in the first schema of the search path, usually `public`. When the sidebar
/// lists nothing there (it's empty, or holds only what extensions installed) but tables elsewhere,
/// the connection starts in the first schema that has some instead (`public` stays in the path).
/// Returns the schema switched to.
fn start_schema(conn: &mut Conn, tables: &[TableRef]) -> Option<String> {
    if conn.kind() != crate::db::Kind::Postgres {
        return None;
    }
    let (_, _, here) = conn.location().ok()?;
    if tables.iter().any(|t| t.schema == here) {
        return None;
    }
    let first = tables.first()?.schema.clone();
    conn.use_schema(&first).ok()?;
    Some(first)
}

/// Spawns the thread that owns the session's connection. Dropping `Spawned::jobs` closes it once
/// the current statement (if any) finishes; `Running::cancel` stops that statement early.
///
/// Postgres and MySQL sessions also get a second connection (unless `background_reads` is off)
/// on a thread of its own, opened once the first is up, for the reads `Job::runs_in_background` lists: a long query in one
/// tab then doesn't hold up the sidebar, an export or the server panel. A dispatcher thread
/// picks the connection for each job; until the second is up (or if it can't connect),
/// everything runs on the first.
pub fn spawn(cfg: ConnConfig, wake: Waker) -> Spawned {
    let (job_tx, job_rx) = channel::<Job>();
    let (front_tx, front_rx) = channel::<Queued>();
    let (reply_tx, reply_rx) = channel::<Tagged>();
    let front = Arc::new(Running::default());
    let back = Arc::new(Running::default());
    let running = vec![front.clone(), back.clone()];
    let cancels = Arc::new(Cancels::default());
    let marks = cancels.clone();
    let shared = Arc::new(Shared::default());
    {
        let shared = shared.clone();
        thread::spawn(move || dispatch(&job_rx, &front_tx, &shared));
    }
    let link = cfg
        .ssh
        .as_ref()
        .map(|ssh| Link::new(ssh.clone(), &cfg.host, cfg.port));
    let session_link = link.clone();
    thread::spawn(move || {
        // The job being run: its replies carry its number.
        let seq = std::cell::Cell::new(0);
        let send = |reply: Reply| {
            // Only fails once the app dropped its end: nobody is left to tell.
            let _ = reply_tx.send(Tagged {
                seq: seq.get(),
                reply,
            });
            wake();
        };
        let via = match route(link.as_ref()) {
            Ok(via) => via,
            Err(e) => return send(Reply::Connected(Err(e))),
        };
        let mut conn = match Conn::connect(&cfg, via) {
            Ok(c) => c,
            Err(e) => return send(Reply::Connected(Err(e))),
        };
        front.set_canceller(conn.canceller(&cfg, via));
        let tables = conn.tables();
        let location = tables.as_ref().ok().and_then(|t| start_schema(&mut conn, t));
        send(Reply::Connected(tables.map(|tables| Connected { tables })));
        if cfg.kind != crate::db::Kind::Sqlite && cfg.background_reads {
            let bg = spawn_background(
                cfg.clone(),
                link.clone(),
                location.clone(),
                back,
                reply_tx.clone(),
                wake.clone(),
                marks.clone(),
            );
            let _ = shared.background.set(bg);
        }
        send(Reply::AllColumns(conn.all_columns()));
        send(Reply::AllFks(conn.all_fks()));
        send(Reply::Objects(conn.objects()));
        send(Reply::Location(conn.location()));
        let mut w = Worker {
            conn,
            tx: TxState {
                manual: cfg.manual_commit,
                ..Default::default()
            },
            pg: cfg.kind == crate::db::Kind::Postgres,
            cfg: &cfg,
            link,
            via,
            location,
            last_job: Instant::now(),
            send: &send,
            seq: &seq,
            running: &front,
            cancels: &marks,
            background: false,
        };
        while let Ok(q) = front_rx.recv() {
            let holds_back = q.job.holds_back_background();
            let schema = match &q.job {
                Job::UseSchema(name) => Some(name.clone()),
                _ => None,
            };
            w.run(q);
            // The second connection follows, before the reads waiting for this go to it.
            if let (Some(name), Some(bg)) = (schema, shared.background.get())
                && w.location.as_ref() == Some(&name)
            {
                bg.send(Queued::now(Job::UseSchema(name)));
            }
            // Stored before the count drops: `dispatch` reads them the other way round.
            shared.tx_open.store(w.tx.open, Ordering::SeqCst);
            if holds_back {
                shared.holding_back.fetch_sub(1, Ordering::SeqCst);
            }
        }
    });
    Spawned {
        jobs: job_tx,
        replies: reply_rx,
        running,
        cancels,
        link: session_link,
    }
}

/// Where a connection goes: straight to the server, or to the tunnel's local port (ssh is started
/// again first if it exited).
fn route(link: Option<&Arc<Link>>) -> DbResult<Option<u16>> {
    link.map(|l| l.port())
        .transpose()
        .map_err(|e| DbError::Other(format!("SSH tunnel: {e}")))
}

/// What the dispatcher and the first connection's thread share.
#[derive(Default)]
struct Shared {
    /// The second connection, once its thread is started.
    background: OnceLock<Background>,
    /// A transaction is open on the first connection.
    tx_open: AtomicBool,
    /// Jobs sent to the first connection that later reads must wait for (see
    /// `Job::holds_back_background`), not finished yet.
    holding_back: AtomicUsize,
}

/// Sends each job to the second connection when it can run there, to the first otherwise.
fn dispatch(jobs: &Receiver<Job>, front: &Sender<Queued>, shared: &Shared) {
    let mut seq = 0;
    while let Ok(job) = jobs.recv() {
        seq += 1;
        let job = Queued {
            seq,
            ..Queued::now(job)
        };
        if let Some(bg) = shared.background.get().filter(|bg| bg.ready())
            && job.job.runs_in_background()
            // The count first: when it reads 0, the thread already stored `tx_open`.
            && shared.holding_back.load(Ordering::SeqCst) == 0
            && !shared.tx_open.load(Ordering::SeqCst)
        {
            bg.send(job);
            continue;
        }
        if job.job.holds_back_background() {
            shared.holding_back.fetch_add(1, Ordering::SeqCst);
        }
        if front.send(job).is_err() {
            return;
        }
    }
}

/// The session's second connection, as the first thread sees it.
struct Background {
    jobs: Sender<Queued>,
    /// Connected, and not lost since.
    ready: Arc<AtomicBool>,
}

impl Background {
    fn ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    fn send(&self, job: Queued) {
        let _ = self.jobs.send(job);
    }
}

/// Opens the second connection on its own thread and runs what the first thread hands it. It
/// ends when the first thread does (its sender is dropped).
fn spawn_background(
    cfg: ConnConfig,
    link: Option<Arc<Link>>,
    mut location: Option<String>,
    running: Arc<Running>,
    reply_tx: Sender<Tagged>,
    wake: Waker,
    cancels: Arc<Cancels>,
) -> Background {
    let (jobs, job_rx) = channel::<Queued>();
    let ready = Arc::new(AtomicBool::new(false));
    let ready_flag = ready.clone();
    thread::spawn(move || {
        let seq = std::cell::Cell::new(0);
        let send = |reply: Reply| {
            let _ = reply_tx.send(Tagged {
                seq: seq.get(),
                reply,
            });
            wake();
        };
        // Until it connects, everything stays on the first connection; it tries again now
        // and then. All it can be sent meanwhile is the schema to follow.
        let (mut conn, via) = loop {
            let attempt =
                route(link.as_ref()).and_then(|via| Ok((Conn::connect_as(&cfg, via, BACKGROUND_NAME)?, via)));
            match attempt {
                Ok(c) => break c,
                Err(_) => match job_rx.recv_timeout(BACKGROUND_RETRY) {
                    Ok(Queued {
                        job: Job::UseSchema(name),
                        ..
                    }) => location = Some(name),
                    Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                },
            }
        };
        if let Some(name) = &location {
            let _ = conn.use_schema(name);
        }
        running.set_canceller(conn.canceller(&cfg, via));
        ready_flag.store(true, Ordering::Relaxed);
        let mut w = Worker {
            conn,
            tx: TxState::default(),
            pg: cfg.kind == crate::db::Kind::Postgres,
            cfg: &cfg,
            link,
            via,
            location,
            last_job: Instant::now(),
            send: &send,
            seq: &seq,
            running: &running,
            cancels: &cancels,
            background: true,
        };
        loop {
            let job = if ready_flag.load(Ordering::Relaxed) {
                job_rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                job_rx.recv_timeout(BACKGROUND_RETRY)
            };
            let ok = match job {
                Ok(job) => w.run(job),
                // Lost and not replaced: try again.
                Err(RecvTimeoutError::Timeout) => w.ensure_alive(true),
                Err(RecvTimeoutError::Disconnected) => return,
            };
            ready_flag.store(ok, Ordering::Relaxed);
        }
    });
    Background { jobs, ready }
}

/// How the second connection names itself on the server, and how long it waits before trying
/// again to connect when it couldn't.
const BACKGROUND_NAME: &str = "tabella (background)";
const BACKGROUND_RETRY: Duration = Duration::from_secs(60);
