//! Application state and the logic that acts on it. The `ui` modules draw this state and turn
//! clicks and keys into calls here; nothing in this file draws anything.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Instant;

use eframe::egui;

use crate::config;
use crate::db::{Canceller, ColumnInfo, ConnConfig, Kind, QueryResult, TableRef, TableStructure};
use crate::export::{self, Format};
use crate::favorites::{self, Favorite};
use crate::history::{self, Entry};
use crate::secrets::{Derived, SecretStore};
use crate::session::{self, SavedTab};
use crate::sql::{self, PageQuery, Stmt};
use crate::values::{self, ValueKind};
use crate::worker::{self, ExportSource, ExportSpec, Job, Reply};

/// A message for the user, outside any tab (connection status, save problems).
pub struct Notice {
    pub text: String,
    pub error: bool,
}

impl Notice {
    fn info(text: impl Into<String>) -> Option<Self> {
        Some(Notice {
            text: text.into(),
            error: false,
        })
    }

    fn error(text: impl Into<String>) -> Option<Self> {
        Some(Notice {
            text: text.into(),
            error: true,
        })
    }
}

/// An open connection. Jobs go to its worker thread; replies come back through `poll`.
pub struct Session {
    pub cfg: ConnConfig,
    tx: Sender<Job>,
    rx: Receiver<Reply>,
    canceller: Option<Arc<Canceller>>,
    pub ready: bool,
    pub tables: Vec<TableRef>,
    /// `(table, column)` pairs for SQL autocomplete.
    pub columns: Vec<(String, String)>,
}

impl Session {
    pub fn send(&self, job: Job) {
        // Only fails when the worker is gone; `poll` reports that.
        let _ = self.tx.send(job);
    }

    /// Asks the server to stop the running statement. Runs on its own thread: it opens a
    /// connection and must not block the UI.
    pub fn cancel(&self) {
        if let Some(c) = self.canceller.clone() {
            std::thread::spawn(move || {
                let _ = c.cancel();
            });
        }
    }
}

pub struct Edit {
    pub row: usize,
    pub col: usize,
    pub text: String,
    /// Value when editing started; unchanged text is not written back.
    pub original: String,
    /// Whether the text box already grabbed keyboard focus.
    pub focused: bool,
}

pub struct TableState {
    pub schema: String,
    pub table: String,
    pub cols: Vec<ColumnInfo>,
    pub page: usize,
    pub filter: String,
    pub order: Option<(String, bool)>,
    pub edit: Option<Edit>,
    /// Exact row count for the current filter, once the COUNT(*) comes back.
    pub total: Option<i64>,
    /// Planner estimate from the table list; decides whether an exact count is affordable.
    pub estimate: Option<i64>,
    /// Filter the current `total` was computed for.
    pub counted_filter: Option<String>,
    /// Grid edits and deletions not saved yet, oldest first (undo pops the last one).
    pub changes: Vec<Change>,
    /// The "save these changes?" summary is open.
    pub confirm_save: bool,
    /// Boolean cell whose true/false/NULL dropdown is open.
    pub bool_pick: Option<(usize, usize)>,
    /// The "new row" form, while open.
    pub insert: Option<Vec<InsertField>>,
}

/// A pending grid change. Rows are identified by their primary key values, so changes survive
/// the page being reloaded (e.g. after inserting a row).
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Set {
        key: Vec<Option<String>>,
        col: String,
        value: Option<String>,
        before: Option<String>,
    },
    Delete {
        key: Vec<Option<String>>,
    },
}

impl Change {
    fn key(&self) -> &[Option<String>] {
        match self {
            Change::Set { key, .. } | Change::Delete { key } => key,
        }
    }
}

/// The page as it will look after saving, plus which cells changed and which rows go away.
pub struct Overlay {
    pub result: QueryResult,
    pub changed: HashSet<(usize, usize)>,
    pub deleted: HashSet<usize>,
}

/// Exact counts are skipped above this many (estimated) rows: COUNT(*) would hold the connection.
const EXACT_COUNT_LIMIT: i64 = 1_000_000;

impl TableState {
    pub(crate) fn new(schema: &str, table: &str, estimate: Option<i64>) -> Self {
        TableState {
            schema: schema.into(),
            table: table.into(),
            cols: vec![],
            page: 0,
            filter: String::new(),
            order: None,
            edit: None,
            total: None,
            estimate,
            counted_filter: None,
            changes: Vec::new(),
            confirm_save: false,
            bool_pick: None,
            insert: None,
        }
    }

    /// Primary key values of a row of the current result.
    pub fn row_key(&self, res: &QueryResult, row: usize) -> Vec<Option<String>> {
        self.pk_names()
            .iter()
            .map(|p| {
                res.columns
                    .iter()
                    .position(|c| c == p)
                    .and_then(|i| res.rows[row][i].clone())
            })
            .collect()
    }

    /// `(edited cells, deleted rows)` among the pending changes.
    pub fn change_counts(&self) -> (usize, usize) {
        let deletes = self
            .changes
            .iter()
            .filter(|c| matches!(c, Change::Delete { .. }))
            .count();
        (self.changes.len() - deletes, deletes)
    }

    /// The current page with pending changes applied, for display. `None` when nothing is pending.
    pub fn overlay(&self, res: &QueryResult) -> Option<Overlay> {
        if self.changes.is_empty() {
            return None;
        }
        let mut out = Overlay {
            result: res.clone(),
            changed: HashSet::new(),
            deleted: HashSet::new(),
        };
        for row in 0..res.rows.len() {
            let key = self.row_key(res, row);
            for c in self.changes.iter().filter(|c| c.key() == key.as_slice()) {
                match c {
                    Change::Delete { .. } => {
                        out.deleted.insert(row);
                    }
                    Change::Set { col, value, .. } => {
                        if let Some(ci) = res.columns.iter().position(|n| n == col) {
                            out.result.rows[row][ci] = value.clone();
                            out.changed.insert((row, ci));
                        }
                    }
                }
            }
        }
        Some(out)
    }

    /// One statement per pending change, in the order they were made.
    pub fn change_statements(&self, kind: Kind) -> Vec<Stmt> {
        let pk = self.pk_names();
        self.changes
            .iter()
            .map(|c| {
                let wh = sql::pk_where(kind, &pk, &pk, c.key());
                match c {
                    Change::Set { col, value, .. } => {
                        sql::update_cell(kind, &self.schema, &self.table, col, value.as_deref(), wh)
                    }
                    Change::Delete { .. } => sql::delete_row(kind, &self.schema, &self.table, wh),
                }
            })
            .collect()
    }

    pub fn pk_names(&self) -> Vec<String> {
        self.cols
            .iter()
            .filter(|c| c.pk)
            .map(|c| c.name.clone())
            .collect()
    }

    pub fn page_sql(&self, kind: Kind) -> String {
        let pk = self.pk_names();
        sql::select_page(
            kind,
            &PageQuery {
                schema: &self.schema,
                table: &self.table,
                filter: &self.filter,
                order: self.order.as_ref().map(|(c, asc)| (c.as_str(), *asc)),
                pk: &pk,
                page: self.page,
            },
        )
    }
}

pub enum TabKind {
    Sql { text: String },
    Table(Box<TableState>),
    Structure(Box<StructState>),
}

/// "Estrutura" tab: columns, indexes, foreign keys and DDL of one table.
pub struct StructState {
    pub schema: String,
    pub table: String,
    pub data: Option<TableStructure>,
}

/// How a column is filled in the "new row" form.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FieldMode {
    /// Left out of the INSERT: the database fills it (DEFAULT, serial, auto_increment...).
    Default,
    Null,
    Value,
}

pub struct InsertField {
    pub name: String,
    pub data_type: String,
    pub kind: ValueKind,
    pub nullable: bool,
    pub mode: FieldMode,
    pub text: String,
}

pub struct Tab {
    pub id: u64,
    pub title: String,
    pub kind: TabKind,
    pub result: Option<QueryResult>,
    pub error: Option<String>,
    /// Jobs sent and not answered yet. The tab is busy while this is above zero.
    pending: u32,
    /// Selected cell `(row, column)` in the result grid.
    pub selected: Option<(usize, usize)>,
    /// SQL sent from an editor tab, recorded in the history once its result arrives.
    pub pending_sql: Option<String>,
    /// Row whose detail panel was closed with its X; the panel comes back on the next click.
    pub detail_hidden: Option<usize>,
    /// SQL waiting for the user to confirm destructive statements.
    pub confirm_run: Option<String>,
    /// Last export of this tab: file and row count, shown until dismissed.
    pub exported: Option<(PathBuf, u64)>,
}

impl Tab {
    pub(crate) fn new(id: u64, title: String, kind: TabKind) -> Self {
        Tab {
            id,
            title,
            kind,
            result: None,
            error: None,
            pending: 0,
            selected: None,
            pending_sql: None,
            detail_hidden: None,
            confirm_run: None,
            exported: None,
        }
    }

    pub fn busy(&self) -> bool {
        self.pending > 0
    }

    /// Sends a job that answers with `Reply::Rows`; the tab stays busy until it does.
    fn send_rows_job(&mut self, s: &Session, job: Job) {
        self.pending += 1;
        s.send(job);
    }
}

/// Ctrl+P command palette: fuzzy search over tables, connections and actions.
#[derive(Default)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
    /// Set once the first frame of this opening has run (results from a previous opening are
    /// dropped then).
    pub started: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MasterMode {
    /// First use: choose a master password (typed twice).
    Create,
    Unlock,
}

/// What to do once the vault is open (or the user chose to go on without it).
#[derive(Clone, PartialEq, Debug)]
pub enum AfterMaster {
    Nothing,
    /// Connect to the saved connection with this id.
    Connect(String),
    /// Persist connections (also moves plain-text passwords from old files into the vault).
    Save,
}

/// Master-password prompt for the encrypted vault (Linux without a system keyring).
pub struct MasterPrompt {
    pub mode: MasterMode,
    pub password: String,
    pub confirm: String,
    pub error: Option<String>,
    pub then: AfterMaster,
    /// Key derivation running off the UI thread (Argon2 takes a noticeable moment).
    pub working: Option<Receiver<Result<Derived, String>>>,
}

/// "Salvar consulta" dialog.
pub struct FavPrompt {
    pub name: String,
    pub sql: String,
    /// Offer it on every connection instead of only the current one.
    pub shared: bool,
}

pub struct ConnForm {
    /// Id of the connection being edited; `None` for a new one.
    pub editing: Option<String>,
    pub cfg: ConnConfig,
}

/// What the grid asks for. Produced by the UI, carried out by `handle_grid_event`.
pub enum GridEvent {
    Sort(String),
    Select(usize, usize),
    StartEdit(usize, usize),
    Commit(usize, usize, Option<String>),
    CancelEdit,
    DeleteRow(usize),
    Copy(usize, CopyAs),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CopyAs {
    Json,
    Insert,
}

pub struct App {
    /// Handle to the UI, for work started outside a frame (e.g. connecting after an unlock).
    ctx: egui::Context,
    pub saved: Vec<ConnConfig>,
    pub secrets: SecretStore,
    pub master: Option<MasterPrompt>,
    pub session: Option<Session>,
    pub form: Option<ConnForm>,
    pub palette: Option<Palette>,
    pub tabs: Vec<Tab>,
    pub active: usize,
    next_id: u64,
    pub filter: String,
    pub notice: Option<Notice>,
    /// Redraw every frame at the monitor's rate (for the FPS readout). Off by default: an idle
    /// app should use no CPU.
    pub max_fps: bool,
    pub history: Vec<Entry>,
    pub favorites: Vec<Favorite>,
    /// Tabs from the last run, reopened once their connection is ready.
    restore: Option<session::Saved>,
    /// Current window size, saved on exit.
    pub window: Option<[f32; 2]>,
    pub fav_prompt: Option<FavPrompt>,
    /// Row detail panel on the right of the grid.
    pub show_detail: bool,
    frames: u32,
    fps_since: Instant,
    pub fps: f32,
}

impl App {
    pub fn new(ctx: &egui::Context) -> Self {
        let secrets = SecretStore::detect();
        let (saved, outdated) = config::load(&secrets);
        let mut app = Self {
            ctx: ctx.clone(),
            saved,
            secrets,
            master: None,
            session: None,
            form: None,
            palette: None,
            tabs: vec![],
            active: 0,
            next_id: 1,
            filter: String::new(),
            notice: None,
            max_fps: false,
            history: history::load(),
            favorites: favorites::load(),
            restore: None,
            window: None,
            fav_prompt: None,
            show_detail: true,
            frames: 0,
            fps_since: Instant::now(),
            fps: 0.0,
        };
        let vault_has_any = app
            .secrets
            .locked_vault()
            .is_some_and(|v| app.saved.iter().any(|c| v.has(&c.id) || v.has(&c.name)));
        if outdated {
            // Plain-text passwords or missing ids from older versions: rewrite the file now.
            app.save_connections();
        } else if vault_has_any {
            app.ask_master(AfterMaster::Nothing);
        }
        // Pick up where the last run stopped: same connection, then its tabs (see `poll`).
        let last = session::load();
        if let Some(id) = last.conn.clone()
            && app.find_saved(&id).is_some()
        {
            app.restore = Some(last);
            app.connect_saved(&id);
        }
        #[cfg(feature = "dev-conn")]
        app.add_dev_connections();
        app
    }

    /// What to restore next time: open connection, tabs and window size.
    pub fn snapshot(&self) -> session::Saved {
        let tabs = self
            .tabs
            .iter()
            .map(|t| match &t.kind {
                TabKind::Sql { text } => SavedTab::Sql { text: text.clone() },
                TabKind::Table(ts) => SavedTab::Table {
                    schema: ts.schema.clone(),
                    table: ts.table.clone(),
                    filter: ts.filter.clone(),
                    order: ts.order.clone(),
                },
                TabKind::Structure(st) => SavedTab::Structure {
                    schema: st.schema.clone(),
                    table: st.table.clone(),
                },
            })
            .collect();
        session::Saved {
            conn: self.session.as_ref().map(|s| s.cfg.id.clone()),
            tabs,
            active: self.active,
            window: self.window,
        }
    }

    fn restore_tabs(&mut self, saved: session::Saved) {
        for t in saved.tabs {
            match t {
                SavedTab::Sql { text } => self.open_sql(text),
                SavedTab::Structure { schema, table } => self.open_structure(&schema, &table),
                SavedTab::Table {
                    schema,
                    table,
                    filter,
                    order,
                } => {
                    self.open_table(&schema, &table);
                    // The first page is only requested when the columns arrive, so these apply.
                    if let Some(TabKind::Table(ts)) = self.tabs.last_mut().map(|t| &mut t.kind) {
                        ts.filter = filter;
                        ts.order = order;
                    }
                }
            }
        }
        self.active = saved.active.min(self.tabs.len().saturating_sub(1));
    }

    /// Test databases from the docker setup, listed first (`dev-conn` feature only: it embeds
    /// their passwords, so release builds leave it off).
    #[cfg(feature = "dev-conn")]
    fn add_dev_connections(&mut self) {
        let mut dev = ConnConfig::new(Kind::Postgres);
        dev.id = "dev".into();
        dev.name = "dev".into();
        dev.password = "postgres".into();
        dev.database = "postgres".into();
        let mut dev_mysql = ConnConfig::new(Kind::MySql);
        dev_mysql.id = "dev-mysql".into();
        dev_mysql.name = "dev-mysql".into();
        dev_mysql.port = 3307;
        dev_mysql.password = "mysql".into();
        dev_mysql.database = "loja".into();
        self.saved
            .retain(|c| c.name != dev.name && c.name != dev_mysql.name);
        self.saved.insert(0, dev_mysql);
        self.saved.insert(0, dev);
        self.connect_saved("dev");
    }

    pub fn tick_fps(&mut self) {
        self.frames += 1;
        let elapsed = self.fps_since.elapsed().as_secs_f32();
        if elapsed >= 0.5 {
            self.fps = self.frames as f32 / elapsed;
            self.frames = 0;
            self.fps_since = Instant::now();
        }
    }

    /// Stores the query from the "Salvar consulta" dialog.
    pub fn add_favorite(&mut self, p: FavPrompt) {
        let conn = if p.shared {
            String::new()
        } else {
            self.session
                .as_ref()
                .map(|s| s.cfg.id.clone())
                .unwrap_or_default()
        };
        self.favorites.push(Favorite {
            id: config::new_id(),
            name: p.name.trim().to_string(),
            sql: p.sql,
            conn,
        });
        self.save_favorites();
    }

    pub fn delete_favorite(&mut self, id: &str) {
        self.favorites.retain(|f| f.id != id);
        self.save_favorites();
    }

    fn save_favorites(&mut self) {
        if let Err(e) = favorites::save(&self.favorites) {
            self.notice = Notice::error(format!("Erro ao salvar favoritos: {e}"));
        }
    }

    pub fn find_saved(&self, id: &str) -> Option<&ConnConfig> {
        self.saved.iter().find(|c| c.id == id)
    }

    /// Saves, first asking for the master password if passwords would otherwise be lost.
    pub fn save_connections(&mut self) {
        let has_passwords = self.saved.iter().any(|c| !c.password.is_empty());
        if has_passwords && self.secrets.locked_vault().is_some() {
            self.ask_master(AfterMaster::Save);
        } else {
            self.write_connections();
        }
    }

    fn write_connections(&mut self) {
        match config::save(&self.saved, &mut self.secrets) {
            Ok(0) => {}
            Ok(n) => {
                self.notice = Notice::info(format!(
                    "{n} senha(s) não foram salvas em disco (só nesta sessão)"
                ))
            }
            Err(e) => self.notice = Notice::error(format!("Erro ao salvar conexões: {e}")),
        }
    }

    /// Adds or replaces (by id) a connection from the form and saves the list.
    pub fn store_connection(&mut self, mut cfg: ConnConfig) {
        match self
            .saved
            .iter()
            .position(|c| c.id == cfg.id && !cfg.id.is_empty())
        {
            Some(i) => self.saved[i] = cfg,
            None => {
                cfg.id = config::new_id();
                self.saved.push(cfg);
            }
        }
        self.save_connections();
    }

    pub fn delete_connection(&mut self, id: &str) {
        let Some(i) = self.saved.iter().position(|c| c.id == id) else {
            return;
        };
        let removed = self.saved.remove(i);
        if self.session.as_ref().is_some_and(|s| s.cfg.id == removed.id) {
            self.disconnect();
        }
        config::forget(&removed, &mut self.secrets);
        self.save_connections();
    }

    pub fn ask_master(&mut self, then: AfterMaster) {
        let Some(v) = self.secrets.locked_vault() else {
            return self.after_master(then);
        };
        let mode = if v.exists() {
            MasterMode::Unlock
        } else {
            MasterMode::Create
        };
        self.master = Some(MasterPrompt {
            mode,
            password: String::new(),
            confirm: String::new(),
            error: None,
            then,
            working: None,
        });
    }

    /// Runs after the vault was opened or the prompt was skipped.
    pub fn after_master(&mut self, then: AfterMaster) {
        config::fill_passwords(&mut self.saved, &self.secrets);
        match then {
            AfterMaster::Nothing => {}
            AfterMaster::Save => self.write_connections(),
            AfterMaster::Connect(id) => self.connect_saved_now(&id),
        }
    }

    /// Connects to a saved connection, unlocking the vault first if its password is in there.
    pub fn connect_saved(&mut self, id: &str) {
        let Some(c) = self.find_saved(id) else { return };
        let needs_vault = c.password.is_empty()
            && self
                .secrets
                .locked_vault()
                .is_some_and(|v| v.has(&c.id) || v.has(&c.name));
        if needs_vault {
            self.ask_master(AfterMaster::Connect(id.to_string()));
        } else {
            self.connect_saved_now(id);
        }
    }

    fn connect_saved_now(&mut self, id: &str) {
        let Some(cfg) = self.find_saved(id).cloned() else {
            return;
        };
        self.disconnect();
        let (tx, rx) = worker::spawn(cfg.clone(), self.ctx.clone());
        self.notice = Notice::info(format!("Conectando a {}…", cfg.name));
        self.session = Some(Session {
            cfg,
            tx,
            rx,
            canceller: None,
            ready: false,
            tables: vec![],
            columns: vec![],
        });
    }

    /// Closes the connection. Refused (returns false) while a tab has unsaved changes.
    pub fn disconnect(&mut self) -> bool {
        if let Some(i) = self.tabs.iter().position(has_unsaved) {
            self.active = i;
            self.notice =
                Notice::error("Há alterações não salvas: salve ou descarte antes de trocar de conexão.");
            return false;
        }
        if let Some(s) = self.session.take() {
            // Don't leave a long query running on the server after we stop listening.
            if self.tabs.iter().any(Tab::busy) {
                s.cancel();
            }
            self.notice = Notice::info("Desconectado");
        }
        self.tabs.clear();
        true
    }

    pub fn poll(&mut self) {
        let Some(s) = &mut self.session else { return };
        let mut restore = None;
        loop {
            let reply = match s.rx.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The worker thread ended unexpectedly (or failed to connect, reported above).
                    if s.ready {
                        self.notice = Notice::error("A conexão foi encerrada inesperadamente.");
                    }
                    self.session = None;
                    for t in &mut self.tabs {
                        t.pending = 0;
                    }
                    return;
                }
            };
            match reply {
                Reply::Connected(Ok(c)) => {
                    if self
                        .restore
                        .as_ref()
                        .is_some_and(|r| r.conn.as_deref() == Some(s.cfg.id.as_str()))
                    {
                        restore = self.restore.take();
                    }
                    s.ready = true;
                    s.tables = c.tables;
                    s.canceller = Some(c.canceller);
                    self.notice = Notice::info(format!("Conectado a {}", s.cfg.name));
                }
                Reply::Connected(Err(e)) => {
                    self.notice = Notice::error(format!("Falha ao conectar: {e}"));
                    self.session = None;
                    return;
                }
                Reply::Tables(Ok(t)) => s.tables = t,
                Reply::Tables(Err(e)) => self.notice = Notice::error(e.to_string()),
                Reply::AllColumns(Ok(c)) => s.columns = c,
                // Autocomplete just has no columns then.
                Reply::AllColumns(Err(_)) => {}
                Reply::Rows { tab, res, notice } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) else {
                        continue;
                    };
                    t.pending = t.pending.saturating_sub(1);
                    if let Some(sql) = t.pending_sql.take() {
                        let (ok, ms) = match &*res {
                            Ok(r) => (true, r.elapsed.as_millis() as u64),
                            Err(_) => (false, 0),
                        };
                        let entry = Entry {
                            conn: s.cfg.name.clone(),
                            sql,
                            at: history::now(),
                            ok,
                            ms,
                        };
                        history::push(&mut self.history, entry);
                    }
                    t.selected = None;
                    match *res {
                        Ok(r) => {
                            t.result = Some(r);
                            t.error = notice;
                        }
                        Err(e) => t.error = Some(e.to_string()),
                    }
                }
                Reply::Count { tab, res } => {
                    if let Some(Tab {
                        kind: TabKind::Table(ts),
                        ..
                    }) = self.tabs.iter_mut().find(|t| t.id == tab)
                    {
                        ts.total = res.ok();
                    }
                }
                Reply::Saved { tab, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) else {
                        continue;
                    };
                    t.pending = t.pending.saturating_sub(1);
                    match *res {
                        Ok(r) => {
                            if let TabKind::Table(ts) = &mut t.kind {
                                ts.changes.clear();
                            }
                            t.result = Some(r);
                            t.selected = None;
                            t.error = None;
                        }
                        // Nothing was saved: the changes stay, so the user can fix and retry.
                        Err(e) => t.error = Some(e.to_string()),
                    }
                }
                Reply::Structure { tab, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) else {
                        continue;
                    };
                    t.pending = t.pending.saturating_sub(1);
                    match (res, &mut t.kind) {
                        (Ok(d), TabKind::Structure(st)) => st.data = Some(d),
                        (Err(e), _) => t.error = Some(e.to_string()),
                        _ => {}
                    }
                }
                Reply::Exported { tab, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) else {
                        continue;
                    };
                    t.pending = t.pending.saturating_sub(1);
                    match res {
                        Ok(done) => t.exported = Some(done),
                        Err(e) => t.error = Some(format!("Exportação falhou: {e}")),
                    }
                }
                Reply::Columns { tab, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) else {
                        continue;
                    };
                    t.pending = t.pending.saturating_sub(1);
                    match (res, &mut t.kind) {
                        (Ok(c), TabKind::Table(ts)) => ts.cols = c,
                        (Err(e), _) => t.error = Some(e.to_string()),
                        _ => {}
                    }
                    // First page waits for the columns so it can be ordered by the primary key.
                    let first_load = matches!(&t.kind, TabKind::Table(ts) if ts.counted_filter.is_none());
                    if first_load && t.result.is_none() {
                        reload(t, s);
                    }
                }
            }
        }
        if let Some(r) = restore {
            self.restore_tabs(r);
        }
    }

    fn push_tab(&mut self, title: String, kind: TabKind) -> usize {
        self.tabs.push(Tab::new(self.next_id, title, kind));
        self.next_id += 1;
        self.active = self.tabs.len() - 1;
        self.active
    }

    pub fn close_tab(&mut self, i: usize) {
        if let Some(t) = self.tabs.get_mut(i)
            && has_unsaved(t)
        {
            t.error = Some(
                "Esta aba tem alterações não salvas: salve (Ctrl+S) ou descarte antes de fechar.".into(),
            );
            self.active = i;
            return;
        }
        if i < self.tabs.len() {
            self.tabs.remove(i);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
        }
    }

    pub fn open_sql(&mut self, text: String) {
        let n = self
            .tabs
            .iter()
            .filter(|t| matches!(t.kind, TabKind::Sql { .. }))
            .count()
            + 1;
        self.push_tab(format!("SQL {n}"), TabKind::Sql { text });
    }

    pub fn open_table(&mut self, schema: &str, table: &str) {
        let Some(s) = &self.session else { return };
        let existing = self
            .tabs
            .iter()
            .position(|t| matches!(&t.kind, TabKind::Table(ts) if ts.schema == schema && ts.table == table));
        if let Some(i) = existing {
            self.active = i;
            return;
        }
        let estimate = s
            .tables
            .iter()
            .find(|t| t.schema == schema && t.name == table)
            .and_then(|t| t.estimate);
        let i = self.push_tab(
            table.into(),
            TabKind::Table(Box::new(TableState::new(schema, table, estimate))),
        );
        let Some(s) = &self.session else { return };
        // The first page is requested when the columns arrive (see `poll`); spinner until then.
        let tab = &mut self.tabs[i];
        tab.pending += 1;
        s.send(Job::Columns {
            tab: tab.id,
            schema: schema.into(),
            table: table.into(),
        });
    }
}

impl App {
    pub fn open_structure(&mut self, schema: &str, table: &str) {
        if self.session.is_none() {
            return;
        }
        let existing = self.tabs.iter().position(
            |t| matches!(&t.kind, TabKind::Structure(st) if st.schema == schema && st.table == table),
        );
        if let Some(i) = existing {
            self.active = i;
            return;
        }
        let st = StructState {
            schema: schema.into(),
            table: table.into(),
            data: None,
        };
        let i = self.push_tab(format!("{table} · estrutura"), TabKind::Structure(Box::new(st)));
        let Some(s) = &self.session else { return };
        let tab = &mut self.tabs[i];
        tab.pending += 1;
        s.send(Job::Structure {
            tab: tab.id,
            schema: schema.into(),
            table: table.into(),
        });
    }
}

/// Writes the tab's rows to a file in the Downloads folder. `whole_table` reads the entire table
/// (with the tab's filter) in chunks; otherwise only what is on screen.
pub fn start_export(tab: &mut Tab, s: &Session, format: Format, whole_table: bool) {
    let k = s.cfg.kind;
    let (base, table_name, cols, source) = match &tab.kind {
        TabKind::Table(ts) if whole_table => {
            if let Err(e) = sql::check_filter(&ts.filter) {
                tab.error = Some(e);
                return;
            }
            let source = ExportSource::Table {
                schema: ts.schema.clone(),
                table: ts.table.clone(),
                filter: ts.filter.clone(),
                order: ts.order.clone(),
                pk: ts.pk_names(),
            };
            (
                ts.table.clone(),
                k.qualified(&ts.schema, &ts.table),
                ts.cols.clone(),
                source,
            )
        }
        _ => {
            let Some(res) = &tab.result else { return };
            let (base, name, cols) = match &tab.kind {
                TabKind::Table(ts) => (
                    ts.table.clone(),
                    k.qualified(&ts.schema, &ts.table),
                    ts.cols.clone(),
                ),
                _ => ("consulta".to_string(), "tabela".to_string(), vec![]),
            };
            (base, name, cols, ExportSource::Rows(res.clone()))
        }
    };
    let path = export::target_path(&base, format);
    tab.exported = None;
    let spec = ExportSpec {
        format,
        path,
        table_name,
        cols,
        source,
    };
    let id = tab.id;
    tab.pending += 1;
    s.send(Job::Export { tab: id, spec });
}

/// Opens the "new row" form: columns with a default start as "default", the rest empty.
pub fn open_insert_form(tab: &mut Tab) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let empty = QueryResult {
        columns: ts.cols.iter().map(|c| c.name.clone()).collect(),
        ..Default::default()
    };
    let kinds = values::column_kinds(&empty, &ts.cols);
    ts.insert = Some(
        ts.cols
            .iter()
            .zip(kinds)
            .map(|(c, kind)| InsertField {
                name: c.name.clone(),
                data_type: c.data_type.clone(),
                kind,
                nullable: c.nullable,
                mode: if c.has_default {
                    FieldMode::Default
                } else {
                    FieldMode::Value
                },
                text: String::new(),
            })
            .collect(),
    );
}

/// INSERTs the form's row. Booleans are normalized per engine like grid edits.
pub fn submit_insert(tab: &mut Tab, s: &Session) {
    let k = s.cfg.kind;
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let Some(fields) = ts.insert.take() else { return };
    let mut values = Vec::new();
    for f in &fields {
        let v = match f.mode {
            FieldMode::Default => continue,
            FieldMode::Null => None,
            FieldMode::Value if f.kind == ValueKind::Bool => match sql::parse_bool(&f.text) {
                Some(b) => Some(sql::bool_value(k, b).to_string()),
                None => {
                    tab.error = Some(format!("{}: \"{}\" não é um booleano", f.name, f.text));
                    ts.insert = Some(fields);
                    return;
                }
            },
            FieldMode::Value => Some(f.text.clone()),
        };
        values.push((f.name.clone(), v));
    }
    let stmt = sql::insert_values(k, &ts.schema, &ts.table, &values);
    exec(tab, s, stmt, true);
}

/// Re-runs the current page query of a table tab.
pub fn reload(tab: &mut Tab, s: &Session) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    if !ts.changes.is_empty() {
        // A reload (page, sort, filter) would show rows the pending changes don't refer to.
        tab.error = Some(format!(
            "Salve (Ctrl+S) ou descarte as {} alteração(ões) pendentes antes de recarregar.",
            ts.changes.len()
        ));
        return;
    }
    if let Err(e) = sql::check_filter(&ts.filter) {
        tab.error = Some(e);
        return;
    }
    tab.selected = None;
    ts.edit = None;
    let sql = ts.page_sql(s.cfg.kind);
    // Queued after the page so rows show up first.
    let needs_count = ts.counted_filter.as_deref() != Some(ts.filter.as_str());
    let id = tab.id;
    tab.send_rows_job(s, Job::Query { tab: id, sql });
    if needs_count && let TabKind::Table(ts) = &mut tab.kind {
        request_count(id, ts, s);
    }
}

fn request_count(tab: u64, ts: &mut TableState, s: &Session) {
    ts.counted_filter = Some(ts.filter.clone());
    ts.total = None;
    if ts.estimate.is_none_or(|n| n < EXACT_COUNT_LIMIT) {
        s.send(Job::Count {
            tab,
            sql: sql::count(s.cfg.kind, &ts.schema, &ts.table, &ts.filter),
        });
    }
}

/// Runs a write statement on the table, then refreshes the current page. Row edits and deletes
/// must touch exactly one row; otherwise the user is told the row changed elsewhere.
pub fn exec(tab: &mut Tab, s: &Session, stmt: Stmt, expect_one: bool) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    if let Err(e) = sql::check_filter(&ts.filter) {
        tab.error = Some(e);
        return;
    }
    let refresh = ts.page_sql(s.cfg.kind);
    // Inserts and deletes change the total.
    request_count(tab.id, ts, s);
    let id = tab.id;
    tab.send_rows_job(
        s,
        Job::Exec {
            tab: id,
            stmt,
            refresh,
            expect_one,
        },
    );
}

/// Starts a query from an SQL editor tab; its result is recorded in the history.
pub fn run_sql(tab: &mut Tab, s: &Session, sql: String) {
    tab.error = None;
    tab.pending_sql = Some(sql.clone());
    let id = tab.id;
    tab.send_rows_job(s, Job::Query { tab: id, sql });
}

/// Carries out what the grid asked for. Returns text to put on the clipboard, if any.
pub fn handle_grid_event(tab: &mut Tab, s: &Session, event: GridEvent, editable: bool) -> Option<String> {
    let k = s.cfg.kind;
    let Some(res) = &tab.result else { return None };
    let no_cols = Vec::new();
    let cols = match &tab.kind {
        TabKind::Table(ts) => &ts.cols,
        TabKind::Sql { .. } | TabKind::Structure(_) => &no_cols,
    };
    let kinds = values::column_kinds(res, cols);

    // Things that work on any result, editable or not.
    match &event {
        GridEvent::Select(row, col) => {
            tab.selected = Some((*row, *col));
            tab.detail_hidden = None;
            if let TabKind::Table(ts) = &mut tab.kind {
                ts.bool_pick = None;
            }
            return None;
        }
        GridEvent::Copy(row, how) => {
            let table = match &tab.kind {
                TabKind::Table(ts) => k.qualified(&ts.schema, &ts.table),
                TabKind::Sql { .. } | TabKind::Structure(_) => "tabela".to_string(),
            };
            return Some(match how {
                CopyAs::Json => values::row_json(res, cols, *row),
                CopyAs::Insert => sql::insert_row(k, &table, &res.columns, &res.rows[*row]),
            });
        }
        _ => {}
    }

    let TabKind::Table(ts) = &mut tab.kind else {
        return None;
    };
    if !editable {
        return None;
    }
    match event {
        GridEvent::Sort(col) => {
            ts.order = match &ts.order {
                Some((c, true)) if *c == col => Some((col, false)),
                Some((c, false)) if *c == col => None,
                _ => Some((col, true)),
            };
            ts.page = 0;
            reload(tab, s);
        }
        // Booleans open a true/false/NULL dropdown; nothing is written until a value is picked.
        GridEvent::StartEdit(row, col) if kinds[col] == ValueKind::Bool => {
            tab.selected = Some((row, col));
            ts.bool_pick = Some((row, col));
        }
        GridEvent::StartEdit(row, col) => {
            let text = res.rows[row][col].clone().unwrap_or_default();
            tab.selected = Some((row, col));
            ts.edit = Some(Edit {
                row,
                col,
                original: text.clone(),
                text,
                focused: false,
            });
        }
        GridEvent::CancelEdit => {
            ts.edit = None;
            ts.bool_pick = None;
        }
        GridEvent::Commit(row, col, val) => {
            ts.edit = None;
            ts.bool_pick = None;
            // Typed booleans ("true", "sim", "0"...) are normalized for the engine.
            let val = match (kinds[col], val) {
                (ValueKind::Bool, Some(v)) => match sql::parse_bool(&v) {
                    Some(b) => Some(sql::bool_value(k, b).to_string()),
                    None => {
                        tab.error = Some(format!("\"{v}\" não é um booleano (use true ou false)"));
                        return None;
                    }
                },
                (_, v) => v,
            };
            // Not written yet: it joins the pending changes (saved together with Ctrl+S).
            let key = ts.row_key(res, row);
            let col_name = res.columns[col].clone();
            let before = res.rows[row][col].clone();
            ts.changes.retain(
                |c| !matches!(c, Change::Set { key: k2, col: c2, .. } if *k2 == key && *c2 == col_name),
            );
            let same = match kinds[col] {
                ValueKind::Bool => {
                    before.as_deref().and_then(sql::parse_bool) == val.as_deref().and_then(sql::parse_bool)
                }
                _ => before == val,
            };
            if !same {
                ts.changes.push(Change::Set {
                    key,
                    col: col_name,
                    value: val,
                    before,
                });
            }
        }
        // Toggles: deleting an already-deleted row restores it.
        GridEvent::DeleteRow(row) => {
            let key = ts.row_key(res, row);
            let before = ts.changes.len();
            ts.changes
                .retain(|c| !matches!(c, Change::Delete { key: k2 } if *k2 == key));
            if ts.changes.len() == before {
                ts.changes.push(Change::Delete { key });
            }
        }
        GridEvent::Select(..) | GridEvent::Copy(..) => {}
    }
    None
}

/// Undoes the most recent pending change.
pub fn undo_change(tab: &mut Tab) {
    if let TabKind::Table(ts) = &mut tab.kind {
        ts.changes.pop();
    }
}

pub fn discard_changes(tab: &mut Tab) {
    if let TabKind::Table(ts) = &mut tab.kind {
        ts.changes.clear();
        ts.confirm_save = false;
    }
}

/// Writes every pending change in one transaction, then reloads the page.
pub fn save_changes(tab: &mut Tab, s: &Session) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    ts.confirm_save = false;
    if ts.changes.is_empty() {
        return;
    }
    let stmts = ts.change_statements(s.cfg.kind);
    let refresh = ts.page_sql(s.cfg.kind);
    let id = tab.id;
    tab.error = None;
    tab.send_rows_job(
        s,
        Job::Batch {
            tab: id,
            stmts,
            refresh,
        },
    );
}

pub fn has_unsaved(tab: &Tab) -> bool {
    matches!(&tab.kind, TabKind::Table(ts) if !ts.changes.is_empty())
}

#[cfg(test)]
pub mod tests {
    use std::sync::mpsc::channel;

    use super::*;

    /// A ready session whose jobs land in the returned receiver instead of a database. Keep the
    /// returned sender alive: when it drops, `poll` treats the worker as gone.
    pub fn fake_session(kind: Kind) -> (Session, Receiver<Job>, Sender<Reply>) {
        let (tx, jobs) = channel();
        let (replies, rx) = channel();
        let mut cfg = ConnConfig::new(kind);
        cfg.id = "t".into();
        cfg.name = "t".into();
        let s = Session {
            cfg,
            tx,
            rx,
            canceller: None,
            ready: true,
            tables: vec![],
            columns: vec![],
        };
        (s, jobs, replies)
    }

    fn table_tab() -> Tab {
        let col = |name: &str, ty: &str, pk| ColumnInfo {
            name: name.into(),
            data_type: ty.into(),
            pk,
            nullable: !pk,
            has_default: false,
        };
        let mut ts = TableState::new("public", "t", None);
        ts.cols = vec![
            col("id", "integer", true),
            col("ativo", "boolean", false),
            col("nome", "text", false),
        ];
        let mut tab = Tab::new(1, "t".into(), TabKind::Table(Box::new(ts)));
        tab.result = Some(QueryResult {
            columns: vec!["id".into(), "ativo".into(), "nome".into()],
            rows: vec![vec![Some("7".into()), Some("f".into()), Some("ana".into())]],
            ..Default::default()
        });
        tab
    }

    #[test]
    fn commit_writes_one_row_and_expects_it() {
        let (s, jobs, _replies) = fake_session(Kind::Postgres);
        let mut tab = table_tab();
        handle_grid_event(&mut tab, &s, GridEvent::Commit(0, 2, Some("bia".into())), true);
        assert!(jobs.try_recv().is_err(), "edits wait for Salvar");
        handle_grid_event(&mut tab, &s, GridEvent::DeleteRow(0), true);
        save_changes(&mut tab, &s);
        let Ok(Job::Batch { stmts, .. }) = jobs.try_recv() else {
            panic!("expected a batch")
        };
        let sql: Vec<String> = stmts.iter().map(|st| st.render(Kind::Postgres)).collect();
        assert_eq!(
            sql,
            [
                "UPDATE \"public\".\"t\" SET \"nome\" = 'bia' WHERE \"id\" = '7'",
                "DELETE FROM \"public\".\"t\" WHERE \"id\" = '7'"
            ]
        );
        assert!(tab.busy());
    }

    #[test]
    fn booleans_open_a_picker_and_are_normalized() {
        let (s, jobs, _replies) = fake_session(Kind::MySql);
        let mut tab = table_tab();
        handle_grid_event(&mut tab, &s, GridEvent::StartEdit(0, 1), true);
        assert!(matches!(&tab.kind, TabKind::Table(ts) if ts.bool_pick == Some((0, 1))));
        assert!(jobs.try_recv().is_err(), "opening the picker writes nothing");
        handle_grid_event(&mut tab, &s, GridEvent::Commit(0, 1, Some("true".into())), true);
        save_changes(&mut tab, &s);
        let Ok(Job::Batch { stmts, .. }) = jobs.try_recv() else {
            panic!()
        };
        assert_eq!(
            stmts[0].with_placeholders().1[0],
            "1",
            "MySQL booleans are written as 1/0"
        );
        handle_grid_event(&mut tab, &s, GridEvent::Commit(0, 1, Some("talvez".into())), true);
        assert!(tab.error.is_some());
    }

    #[test]
    fn read_only_tabs_ignore_edits_but_still_copy() {
        let (s, jobs, _replies) = fake_session(Kind::Postgres);
        let mut tab = table_tab();
        handle_grid_event(&mut tab, &s, GridEvent::Commit(0, 2, Some("x".into())), false);
        assert!(jobs.try_recv().is_err());
        let json = handle_grid_event(&mut tab, &s, GridEvent::Copy(0, CopyAs::Json), false).unwrap();
        assert!(json.contains("\"id\": 7"));
    }

    #[test]
    fn unsafe_filters_never_reach_the_database() {
        let (s, jobs, _replies) = fake_session(Kind::Postgres);
        let mut tab = table_tab();
        if let TabKind::Table(ts) = &mut tab.kind {
            ts.filter = "1=1; DELETE FROM t; SELECT 1 AS id".into();
        }
        reload(&mut tab, &s);
        handle_grid_event(&mut tab, &s, GridEvent::DeleteRow(0), true);
        assert!(jobs.try_recv().is_err());
        assert!(tab.error.is_some() && !tab.busy());
    }

    #[test]
    fn insert_form_sends_only_what_was_filled() {
        let (s, jobs, _replies) = fake_session(Kind::MySql);
        let mut tab = table_tab();
        open_insert_form(&mut tab);
        let TabKind::Table(ts) = &mut tab.kind else {
            unreachable!()
        };
        let fields = ts.insert.as_mut().unwrap();
        // id has no default in the fixture: starts as a value to fill in.
        assert_eq!(fields[0].mode, FieldMode::Value);
        fields[0].text = "8".into();
        fields[1].mode = FieldMode::Value;
        fields[1].text = "sim".into();
        fields[2].mode = FieldMode::Default;
        submit_insert(&mut tab, &s);
        let _count = jobs.try_recv().unwrap();
        let Job::Exec { stmt, .. } = jobs.try_recv().unwrap() else {
            panic!()
        };
        let (sql, values) = stmt.with_placeholders();
        assert_eq!(sql, "INSERT INTO `public`.`t` (`id`, `ativo`) VALUES (?, ?)");
        assert_eq!(values, ["8", "1"]);
    }

    #[test]
    fn a_bad_boolean_keeps_the_form_open() {
        let (s, jobs, _replies) = fake_session(Kind::Postgres);
        let mut tab = table_tab();
        open_insert_form(&mut tab);
        if let TabKind::Table(ts) = &mut tab.kind {
            let f = &mut ts.insert.as_mut().unwrap()[1];
            f.mode = FieldMode::Value;
            f.text = "talvez".into();
        }
        submit_insert(&mut tab, &s);
        assert!(jobs.try_recv().is_err());
        assert!(tab.error.is_some());
        assert!(matches!(&tab.kind, TabKind::Table(ts) if ts.insert.is_some()));
    }

    #[test]
    fn exports_go_to_the_worker() {
        let (s, jobs, _replies) = fake_session(Kind::Postgres);
        let mut tab = table_tab();
        start_export(&mut tab, &s, Format::Csv, true);
        let Ok(Job::Export { spec, .. }) = jobs.try_recv() else {
            panic!("expected an export job")
        };
        assert!(matches!(spec.source, ExportSource::Table { .. }));
        assert_eq!(spec.path.extension().unwrap(), "csv");
        assert!(tab.busy());
        start_export(&mut tab, &s, Format::Json, false);
        let Ok(Job::Export { spec, .. }) = jobs.try_recv() else {
            panic!()
        };
        assert!(matches!(spec.source, ExportSource::Rows(ref r) if r.rows.len() == 1));
    }

    #[test]
    fn sort_cycles_and_reloads() {
        let (s, jobs, _replies) = fake_session(Kind::Postgres);
        let mut tab = table_tab();
        for expected in [Some(true), Some(false), None] {
            handle_grid_event(&mut tab, &s, GridEvent::Sort("nome".into()), true);
            let TabKind::Table(ts) = &tab.kind else {
                unreachable!()
            };
            assert_eq!(ts.order.as_ref().map(|(_, asc)| *asc), expected);
        }
        assert!(jobs.try_iter().any(|j| matches!(j, Job::Query { .. })));
    }
}

#[cfg(test)]
mod poll_tests {
    use super::tests::fake_session;
    use super::*;

    fn app_with(session: Session) -> App {
        let ctx = egui::Context::default();
        let dir = std::env::temp_dir().join("dblite-poll-tests-config");
        // SAFETY: tests only point the config at a throwaway directory.
        unsafe { std::env::set_var("DBLITE_CONFIG_DIR", &dir) };
        let mut app = App::new(&ctx);
        app.session = Some(session);
        app
    }

    #[test]
    fn a_dead_worker_ends_the_session_with_an_error() {
        let (s, _jobs, replies) = fake_session(Kind::Postgres);
        let mut app = app_with(s);
        app.open_sql(String::new());
        app.tabs[0].pending = 1;
        drop(replies);
        app.poll();
        assert!(app.session.is_none());
        assert!(app.notice.as_ref().is_some_and(|n| n.error));
        assert!(!app.tabs[0].busy(), "nothing stays stuck on Carregando…");
    }

    #[test]
    fn session_snapshot_restores_the_same_tabs() {
        let (s, _jobs, _replies) = fake_session(Kind::Postgres);
        let mut app = app_with(s);
        app.open_sql("select 42".into());
        app.open_table("public", "clientes");
        if let TabKind::Table(ts) = &mut app.tabs[1].kind {
            ts.filter = "ativo".into();
            ts.order = Some(("nome".into(), false));
        }
        app.open_structure("public", "pedidos");
        app.active = 1;
        let snap = app.snapshot();
        assert_eq!(snap.conn.as_deref(), Some("t"));

        let (s2, jobs2, _r2) = fake_session(Kind::Postgres);
        let mut fresh = app_with(s2);
        fresh.restore_tabs(snap);
        assert_eq!(fresh.snapshot().tabs, app.snapshot().tabs);
        assert_eq!(fresh.active, 1);
        // Restored tabs load their data like freshly opened ones.
        let kinds: Vec<_> = jobs2
            .try_iter()
            .map(|j| matches!(j, Job::Columns { .. } | Job::Structure { .. }))
            .collect();
        assert_eq!(kinds, [true, true]);
    }

    #[test]
    fn a_zero_row_edit_is_reported() {
        let (s, _jobs, replies) = fake_session(Kind::Postgres);
        let mut app = app_with(s);
        app.open_sql(String::new());
        let id = app.tabs[0].id;
        app.tabs[0].pending = 1;
        let notice = Some("Nenhuma linha foi alterada".to_string());
        replies
            .send(Reply::Rows {
                tab: id,
                res: Box::new(Ok(QueryResult::default())),
                notice,
            })
            .unwrap();
        app.poll();
        assert!(
            app.tabs[0]
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("Nenhuma linha"))
        );
        assert!(!app.tabs[0].busy() && app.session.is_some());
    }
}
