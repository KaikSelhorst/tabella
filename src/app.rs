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
use crate::design::Design;
use crate::export::{self, Format};
use crate::favorites::{self, Favorite};
use crate::filters::{self, SavedFilter};
use crate::history::{self, Entry};
use crate::import;
use crate::plan::Plan;
use crate::secrets::{Derived, SecretStore};
use crate::session::{self, SavedTab};
use crate::sql::{self, FilterOp, PageQuery, Stmt};
use crate::values::{self, CellRange, ValueKind};
use crate::worker::{self, ExportSource, ExportSpec, Job, Reply, TxState};

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
    /// Manual-commit mode and the open transaction, as last reported by the worker.
    pub txn: TxState,
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
    /// Text searched in every column (the search box), on top of `filter`.
    pub search: String,
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
    /// The CSV import dialog, while open.
    pub import: Option<ImportForm>,
}

/// "Importar CSV" dialog: a file, how to read it, and where each of its columns goes.
#[derive(Default)]
pub struct ImportForm {
    pub path: String,
    pub has_header: bool,
    pub empty_is_null: bool,
    /// Preview of `path` as last read (path and header flag it was read with).
    pub preview: Option<(String, bool, Result<import::Preview, String>)>,
    /// For each file column, the table column it goes into.
    pub mapping: Vec<Option<String>>,
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
            search: String::new(),
            order: None,
            edit: None,
            total: None,
            estimate,
            counted_filter: None,
            changes: Vec::new(),
            confirm_save: false,
            bool_pick: None,
            insert: None,
            import: None,
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

    /// The WHERE box plus the search box, as one condition.
    pub fn effective_filter(&self, kind: Kind) -> String {
        let names: Vec<String> = self.cols.iter().map(|c| c.name.clone()).collect();
        sql::and(&self.filter, &sql::search_condition(kind, &names, &self.search))
    }

    pub fn page_sql(&self, kind: Kind) -> String {
        let pk = self.pk_names();
        let filter = self.effective_filter(kind);
        sql::select_page(
            kind,
            &PageQuery {
                schema: &self.schema,
                table: &self.table,
                filter: &filter,
                order: self.order.as_ref().map(|(c, asc)| (c.as_str(), *asc)),
                pk: &pk,
                page: self.page,
            },
        )
    }
}

pub enum TabKind {
    Sql {
        text: String,
    },
    Table(Box<TableState>),
    Structure(Box<StructState>),
    Activity(ActivityState),
    /// "Nova tabela" / "Editar estrutura".
    Design(Box<Design>),
}

/// "Atividade" tab: sessions on the server, optionally refreshed every few seconds.
pub struct ActivityState {
    pub auto_refresh: bool,
    pub last_refresh: Option<Instant>,
    /// Session waiting for the user to confirm `(id, terminate)`.
    pub confirm_kill: Option<(i64, bool)>,
    /// Why the last cancel/terminate had no effect (kept until the next one).
    pub kill_error: Option<String>,
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
    /// Other corner of a range selection (Shift+click, drag, Shift+arrows); `selected` is the
    /// corner that moves.
    pub anchor: Option<(usize, usize)>,
    /// SQL sent from an editor tab, recorded in the history once its result arrives.
    pub pending_sql: Option<String>,
    /// Row whose detail panel was closed with its X; the panel comes back on the next click.
    pub detail_hidden: Option<usize>,
    /// SQL waiting for the user to confirm destructive statements.
    pub confirm_run: Option<String>,
    /// Last export of this tab: file and row count, shown until dismissed.
    pub exported: Option<(PathBuf, u64)>,
    /// A script's result sets when it returned more than one, in order. The one on screen lives
    /// in `result`; its slot here is an empty placeholder.
    pub sets: Vec<QueryResult>,
    pub set_shown: usize,
    /// EXPLAIN of an editor statement, shown in place of the result until closed.
    pub plan: Option<Plan>,
    /// Values asked for `:parameters` before running.
    pub params: Option<ParamPrompt>,
    /// Last value typed for each parameter name.
    pub param_values: std::collections::HashMap<String, String>,
    pub col_stats: Option<StatsView>,
    /// Columns hidden from the grid, and columns pinned to its left edge (by name, so they
    /// survive reloads and paging).
    pub hidden_cols: Vec<String>,
    pub pinned_cols: Vec<String>,
}

/// "Estatísticas da coluna" dialog.
pub struct StatsView {
    pub column: String,
    /// What the numbers cover: the whole (filtered) table or just the loaded rows.
    pub scope: String,
    /// `None` while the database is still counting.
    pub data: Option<Result<values::ColumnStats, String>>,
}

pub struct ParamPrompt {
    pub sql: String,
    pub names: Vec<String>,
    /// `Some(analyze)` when the prompt came from "Explicar".
    pub explain: Option<bool>,
    pub focused: bool,
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
            anchor: None,
            pending_sql: None,
            detail_hidden: None,
            confirm_run: None,
            exported: None,
            sets: Vec::new(),
            set_shown: 0,
            plan: None,
            params: None,
            param_values: Default::default(),
            col_stats: None,
            hidden_cols: Vec::new(),
            pinned_cols: Vec::new(),
        }
    }

    /// The selected rectangle (on-screen column order), when more than one cell is selected.
    pub fn range(&self) -> Option<CellRange> {
        let ((ar, ac), (sr, sc)) = (self.anchor?, self.selected?);
        if (ar, ac) == (sr, sc) {
            return None;
        }
        let view = self.view();
        let (pa, ps) = (
            view.iter().position(|&c| c == ac)?,
            view.iter().position(|&c| c == sc)?,
        );
        Some(CellRange {
            r0: ar.min(sr),
            r1: ar.max(sr),
            cols: view[pa.min(ps)..=pa.max(ps)].to_vec(),
        })
    }

    /// The range, or just the selected cell.
    pub fn selection(&self) -> Option<CellRange> {
        let (r, c) = self.selected?;
        Some(self.range().unwrap_or(CellRange::single(r, c)))
    }

    /// Result columns in on-screen order: pinned first, hidden ones left out.
    pub fn view(&self) -> Vec<usize> {
        let Some(res) = &self.result else {
            return Vec::new();
        };
        let pos = |name: &String| res.columns.iter().position(|c| c == name);
        let mut view: Vec<usize> = self
            .pinned_cols
            .iter()
            .filter(|n| !self.hidden_cols.contains(n))
            .filter_map(pos)
            .collect();
        let rest: Vec<usize> = (0..res.columns.len())
            .filter(|&i| !view.contains(&i) && !self.hidden_cols.contains(&res.columns[i]))
            .collect();
        view.extend(rest);
        // Hiding everything would leave nothing to click to bring columns back.
        if view.is_empty() {
            view = (0..res.columns.len()).collect();
        }
        view
    }

    /// How many leading columns of `view()` stay in place when scrolling sideways.
    pub fn pinned_count(&self) -> usize {
        let Some(res) = &self.result else { return 0 };
        self.pinned_cols
            .iter()
            .filter(|n| !self.hidden_cols.contains(n) && res.columns.contains(n))
            .count()
    }

    /// Puts result set `i` of a multi-statement script on screen.
    pub fn show_set(&mut self, i: usize) {
        let Some(current) = self.result.as_mut() else {
            return;
        };
        if i == self.set_shown || i >= self.sets.len() {
            return;
        }
        std::mem::swap(current, &mut self.sets[self.set_shown]);
        std::mem::swap(current, &mut self.sets[i]);
        self.set_shown = i;
        self.selected = None;
        self.anchor = None;
        self.detail_hidden = None;
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
    /// Add a condition on this cell's column and value to the table's filter.
    Filter(usize, usize, FilterOp),
    /// Open the row a foreign-key cell points to.
    FollowFk(usize, usize),
    /// Move the selection's moving corner here, keeping the other one (range selection).
    Extend(usize, usize),
    SelectAll,
    /// Copy the selected cells as tab-separated text, optionally with the column names.
    CopyCells {
        headers: bool,
    },
    /// Spreadsheet text pasted at the selection: each cell becomes a pending edit.
    Paste(String),
    /// Profile of a column: counts, distinct values, range, most common values.
    ColumnStats(usize),
    /// Hide a shown column, or show a hidden one.
    ToggleColumn(usize),
    ShowAllColumns,
    /// Pin a column to the left edge, or unpin it.
    TogglePin(usize),
    /// Open the insert form filled with this row.
    DuplicateRow(usize),
}

/// What `handle_grid_event` needs the app (or the clipboard) to do.
pub enum Effect {
    Copy(String),
    OpenFiltered {
        schema: String,
        table: String,
        filter: String,
    },
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
    pub filters: Vec<SavedFilter>,
    /// Tabs from the last run, reopened once their connection is ready.
    restore: Option<session::Saved>,
    /// Current window size, saved on exit.
    pub window: Option<[f32; 2]>,
    pub fav_prompt: Option<FavPrompt>,
    /// Row detail panel on the right of the grid.
    pub show_detail: bool,
    /// Sidebar lists tables by size (biggest first) instead of by name.
    pub sort_by_size: bool,
    /// The window was asked to close with a transaction open: ask what to do with it.
    pub quit_prompt: bool,
    /// F1 shortcuts screen.
    pub help: bool,
    /// Close the window once the transaction has been ended.
    pub quit_after_tx: bool,
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
            filters: filters::load(),
            restore: None,
            window: None,
            fav_prompt: None,
            show_detail: true,
            sort_by_size: false,
            quit_prompt: false,
            help: false,
            quit_after_tx: false,
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
            .filter_map(|t| match &t.kind {
                TabKind::Activity(_) | TabKind::Design(_) => None,
                TabKind::Sql { text } => Some(SavedTab::Sql { text: text.clone() }),
                TabKind::Table(ts) => Some(SavedTab::Table {
                    schema: ts.schema.clone(),
                    table: ts.table.clone(),
                    filter: ts.filter.clone(),
                    order: ts.order.clone(),
                    hidden: t.hidden_cols.clone(),
                    pinned: t.pinned_cols.clone(),
                }),
                TabKind::Structure(st) => Some(SavedTab::Structure {
                    schema: st.schema.clone(),
                    table: st.table.clone(),
                }),
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
                    hidden,
                    pinned,
                } => {
                    self.open_table(&schema, &table);
                    // The first page is only requested when the columns arrive, so these apply.
                    if let Some(t) = self.tabs.last_mut() {
                        t.hidden_cols = hidden;
                        t.pinned_cols = pinned;
                        if let TabKind::Table(ts) = &mut t.kind {
                            ts.filter = filter;
                            ts.order = order;
                        }
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

    /// Saves `filter` under `name` for a table of the current connection (replacing one with
    /// the same name).
    pub fn save_filter(&mut self, schema: &str, table: &str, name: &str, filter: &str) {
        let Some(conn) = self.session.as_ref().map(|s| s.cfg.id.clone()) else {
            return;
        };
        let name = name.trim();
        self.filters
            .retain(|f| !(f.conn == conn && f.schema == schema && f.table == table && f.name == name));
        self.filters.push(SavedFilter {
            id: config::new_id(),
            name: name.to_string(),
            filter: filter.trim().to_string(),
            conn,
            schema: schema.to_string(),
            table: table.to_string(),
        });
        self.store_filters();
    }

    pub fn delete_filter(&mut self, id: &str) {
        self.filters.retain(|f| f.id != id);
        self.store_filters();
    }

    fn store_filters(&mut self) {
        if let Err(e) = filters::save(&self.filters) {
            self.notice = Notice::error(format!("Erro ao salvar filtros: {e}"));
        }
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
        // Refused with unsaved changes or an open transaction: stay on the current connection.
        if !self.disconnect() {
            return;
        }
        let cfg_manual = cfg.manual_commit;
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
            txn: TxState {
                manual: cfg_manual,
                ..Default::default()
            },
        });
    }

    /// Closes the connection. Refused (returns false) while a tab has unsaved changes.
    pub fn disconnect(&mut self) -> bool {
        if self.session.as_ref().is_some_and(|s| s.txn.open) {
            self.notice =
                Notice::error("Há uma transação aberta: confirme ou desfaça antes de trocar de conexão.");
            return false;
        }
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
        // Design tabs whose DDL ran: (tab, schema, table).
        let mut designed: Vec<(u64, String, String)> = Vec::new();
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
                Reply::Tx(state) => s.txn = state,
                Reply::ColumnStats { tab, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) else {
                        continue;
                    };
                    t.pending = t.pending.saturating_sub(1);
                    if let Some(v) = &mut t.col_stats {
                        v.data = Some(
                            res.map_err(|e| e.to_string())
                                .and_then(|(a, b)| values::stats_from_queries(&a, &b)),
                        );
                    }
                }
                Reply::TxEnded { commit, res } => {
                    if std::mem::take(&mut self.quit_after_tx) && res.is_ok() {
                        self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    self.notice = match res {
                        Ok(()) if commit => Notice::info("Transação confirmada"),
                        Ok(()) => Notice::info("Transação desfeita"),
                        Err(e) => Notice::error(format!("Transação: {e}")),
                    };
                    // What's on screen may include undone changes: reload table pages.
                    for t in &mut self.tabs {
                        if matches!(t.kind, TabKind::Table(_)) && !t.busy() && !has_unsaved(t) {
                            reload(t, s);
                        }
                    }
                }
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
                    // The activity list refreshes every few seconds: keep the selected session
                    // selected (by id) instead of dropping the selection each time.
                    let keep = match (&t.kind, t.selected, &t.result, &*res) {
                        (TabKind::Activity(_), Some((row, col)), Some(old), Ok(new)) => {
                            let id = old.rows.get(row).and_then(|r| r.first()).cloned();
                            new.rows
                                .iter()
                                .position(|r| r.first() == id.as_ref())
                                .map(|r| (r, col))
                        }
                        _ => None,
                    };
                    t.selected = keep;
                    t.anchor = None;
                    match *res {
                        Ok(mut r) => {
                            let earlier = std::mem::take(&mut r.previous);
                            t.sets.clear();
                            if !earlier.is_empty() {
                                t.sets = earlier;
                                t.sets.push(QueryResult::default());
                                t.set_shown = t.sets.len() - 1;
                            }
                            t.result = Some(r);
                            if let TabKind::Design(d) = &t.kind {
                                designed.push((
                                    t.id,
                                    d.schema.trim().to_string(),
                                    d.table.trim().to_string(),
                                ));
                            }
                            match &mut t.kind {
                                // Refreshes every 2 s must not wipe the outcome of a kill.
                                TabKind::Activity(st) => {
                                    t.error = None;
                                    if notice.is_some() {
                                        st.kill_error = notice;
                                    }
                                }
                                _ => t.error = notice,
                            }
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
                Reply::Imported { tab, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) else {
                        continue;
                    };
                    t.pending = t.pending.saturating_sub(1);
                    match res {
                        Ok(n) => {
                            self.notice = Notice::info(format!("{n} linha(s) importada(s)"));
                            if let TabKind::Table(ts) = &mut t.kind {
                                ts.import = None;
                            }
                            reload(t, s);
                            let id = t.id;
                            if let TabKind::Table(ts) = &mut t.kind {
                                request_count(id, ts, s);
                            }
                        }
                        // The dialog stays open to fix the mapping and retry.
                        Err(e) => t.error = Some(format!("Importação falhou: {e}")),
                    }
                }
                Reply::Plan { tab, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) else {
                        continue;
                    };
                    t.pending = t.pending.saturating_sub(1);
                    match res {
                        Ok(p) => t.plan = Some(p),
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
        for (tab, schema, table) in designed {
            self.after_design(tab, &schema, &table);
        }
    }

    /// The table was created or altered: refresh the sidebar and the tabs showing it, and swap
    /// the design tab for the new structure.
    fn after_design(&mut self, tab: u64, schema: &str, table: &str) {
        let Some(s) = &self.session else { return };
        s.send(Job::Tables);
        self.notice = Notice::info(format!("Estrutura de {table} aplicada"));
        for t in &mut self.tabs {
            match &mut t.kind {
                TabKind::Structure(st) if st.schema == schema && st.table == table => {
                    t.pending += 1;
                    s.send(Job::Structure {
                        tab: t.id,
                        schema: schema.into(),
                        table: table.into(),
                    });
                }
                TabKind::Table(ts) if ts.schema == schema && ts.table == table && ts.changes.is_empty() => {
                    // New columns: their info first, then the page (SELECT * picks them up).
                    t.pending += 1;
                    s.send(Job::Columns {
                        tab: t.id,
                        schema: schema.into(),
                        table: table.into(),
                    });
                    reload(t, s);
                }
                _ => {}
            }
        }
        if let Some(i) = self.tabs.iter().position(|t| t.id == tab) {
            self.tabs.remove(i);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
        }
        self.open_structure(schema, table);
    }

    /// "Nova tabela" in the first schema of the connection (or the one given).
    pub fn open_new_table(&mut self, schema: Option<&str>) {
        let Some(s) = &self.session else { return };
        let schema = schema
            .map(str::to_string)
            .or_else(|| s.tables.first().map(|t| t.schema.clone()))
            .unwrap_or_else(|| match s.cfg.kind {
                Kind::Postgres => "public".to_string(),
                Kind::MySql => s.cfg.database.clone(),
            });
        let d = Design::new_table(s.cfg.kind, &schema);
        self.push_tab("nova tabela".into(), TabKind::Design(Box::new(d)));
    }

    /// "Editar estrutura" of a table whose structure was loaded.
    pub fn open_design(&mut self, schema: &str, table: &str, data: &TableStructure) {
        let Some(s) = &self.session else { return };
        if let Some(i) = self.tabs.iter().position(
            |t| matches!(&t.kind, TabKind::Design(d) if d.existing && d.schema == schema && d.table == table),
        ) {
            self.active = i;
            return;
        }
        let d = Design::from_structure(s.cfg.kind, schema, table, data);
        self.push_tab(format!("{table} · editar"), TabKind::Design(Box::new(d)));
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

    /// Confirms or undoes the open transaction (manual-commit mode).
    pub fn end_tx(&mut self, commit: bool) {
        if let Some(s) = &self.session {
            s.send(Job::EndTx { commit });
        }
    }

    /// Switches manual-commit mode for this session; refused while a transaction is open.
    pub fn set_manual_commit(&mut self, on: bool) {
        let Some(s) = &mut self.session else { return };
        if s.txn.open && !on {
            self.notice =
                Notice::error("Confirme ou desfaça a transação aberta antes de voltar ao commit automático.");
            return;
        }
        s.txn.manual = on;
        s.send(Job::SetManual(on));
    }

    /// Opens (or focuses) the server activity monitor.
    pub fn open_activity(&mut self) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| matches!(t.kind, TabKind::Activity(_)))
        {
            self.active = i;
            return;
        }
        if self.session.is_none() {
            return;
        }
        let state = ActivityState {
            auto_refresh: true,
            last_refresh: None,
            confirm_kill: None,
            kill_error: None,
        };
        let i = self.push_tab("Atividade".into(), TabKind::Activity(state));
        if let Some(s) = &self.session {
            refresh_activity(&mut self.tabs[i], s);
        }
    }

    /// Opens (or focuses) a table tab showing only the rows matching `filter`.
    pub fn open_table_filtered(&mut self, schema: &str, table: &str, filter: String) {
        let existing = self
            .tabs
            .iter()
            .position(|t| matches!(&t.kind, TabKind::Table(ts) if ts.schema == schema && ts.table == table));
        let Some(i) = existing else {
            self.open_table(schema, table);
            // Applied before the first page loads (that waits for the columns).
            if let Some(TabKind::Table(ts)) = self.tabs.last_mut().map(|t| &mut t.kind) {
                ts.filter = filter;
            }
            return;
        };
        self.active = i;
        let (Some(s), tab) = (&self.session, &mut self.tabs[i]) else {
            return;
        };
        if let TabKind::Table(ts) = &mut tab.kind {
            if !ts.changes.is_empty() {
                tab.error =
                    Some("Esta aba tem alterações pendentes: salve ou descarte antes de filtrar.".into());
                return;
            }
            ts.filter = filter;
            ts.search.clear();
            ts.page = 0;
        }
        reload(tab, s);
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
                filter: ts.effective_filter(k),
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

/// "Duplicar linha": the insert form filled with a row's values. Keys the database generates
/// stay on "Padrão", so the copy gets a new one.
pub fn open_duplicate_form(tab: &mut Tab, row: usize) {
    open_insert_form(tab);
    let Some(res) = &tab.result else { return };
    let TabKind::Table(ts) = &mut tab.kind else { return };
    // Pending edits included: the copy is of what the grid shows.
    let overlay = ts.overlay(res);
    let shown = overlay.as_ref().map_or(res, |o| &o.result);
    let Some(values) = shown.rows.get(row) else { return };
    let generated: Vec<&str> = ts
        .cols
        .iter()
        .filter(|c| c.pk && c.has_default)
        .map(|c| c.name.as_str())
        .collect();
    let Some(fields) = &mut ts.insert else { return };
    for f in fields.iter_mut() {
        if generated.contains(&f.name.as_str()) {
            continue;
        }
        let Some(ci) = shown.columns.iter().position(|c| *c == f.name) else {
            continue;
        };
        match &values[ci] {
            Some(v) => {
                f.mode = FieldMode::Value;
                f.text = values::display_bool_or(f.kind, v);
            }
            None if f.nullable => f.mode = FieldMode::Null,
            None => {}
        }
    }
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

pub fn refresh_activity(tab: &mut Tab, s: &Session) {
    let TabKind::Activity(st) = &mut tab.kind else {
        return;
    };
    st.last_refresh = Some(Instant::now());
    let id = tab.id;
    tab.send_rows_job(s, Job::Activity { tab: id });
}

/// Cancels (or terminates) the session confirmed in the dialog.
pub fn kill_session(tab: &mut Tab, s: &Session) {
    let TabKind::Activity(st) = &mut tab.kind else {
        return;
    };
    let Some((id, terminate)) = st.confirm_kill.take() else {
        return;
    };
    st.kill_error = None;
    st.last_refresh = Some(Instant::now());
    let tab_id = tab.id;
    tab.send_rows_job(
        s,
        Job::Kill {
            tab: tab_id,
            id,
            terminate,
        },
    );
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
    let needs_count = ts.counted_filter.as_deref() != Some(ts.effective_filter(s.cfg.kind).as_str());
    let id = tab.id;
    tab.send_rows_job(s, Job::Query { tab: id, sql });
    if needs_count && let TabKind::Table(ts) = &mut tab.kind {
        request_count(id, ts, s);
    }
}

fn request_count(tab: u64, ts: &mut TableState, s: &Session) {
    let filter = ts.effective_filter(s.cfg.kind);
    ts.counted_filter = Some(filter.clone());
    ts.total = None;
    if ts.estimate.is_none_or(|n| n < EXACT_COUNT_LIMIT) {
        s.send(Job::Count {
            tab,
            sql: sql::count(s.cfg.kind, &ts.schema, &ts.table, &filter),
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
    tab.plan = None;
    tab.pending_sql = Some(sql.clone());
    let id = tab.id;
    tab.send_rows_job(s, Job::Query { tab: id, sql });
}

/// Opens the CSV import dialog, with `path` already chosen when a file was dropped on the window.
pub fn open_import(tab: &mut Tab, path: Option<String>) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let mut form = ts.import.take().unwrap_or(ImportForm {
        has_header: true,
        empty_is_null: true,
        ..Default::default()
    });
    if let Some(p) = path {
        form.path = p;
    }
    ts.import = Some(form);
}

/// Re-reads the file preview when the path or the header option changed, and guesses the
/// mapping from the column names.
pub fn refresh_import_preview(ts: &mut TableState) {
    let Some(form) = &mut ts.import else { return };
    let path = form.path.trim().trim_matches('"').to_string();
    if path.is_empty() {
        form.preview = None;
        form.mapping.clear();
        return;
    }
    if form
        .preview
        .as_ref()
        .is_some_and(|(p, h, _)| *p == path && *h == form.has_header)
    {
        return;
    }
    let pv = import::preview(std::path::Path::new(&path), form.has_header);
    let names: Vec<String> = ts.cols.iter().map(|c| c.name.clone()).collect();
    form.mapping = match &pv {
        Ok(p) => import::auto_mapping(&p.headers, &names),
        Err(_) => Vec::new(),
    };
    form.preview = Some((path, form.has_header, pv));
}

/// Starts the import as set up in the dialog.
pub fn submit_import(tab: &mut Tab, s: &Session) {
    if s.cfg.read_only {
        tab.error = Some("Conexão somente leitura: não é possível importar.".into());
        return;
    }
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let Some(form) = &ts.import else { return };
    let Some((path, _, Ok(pv))) = &form.preview else {
        return;
    };
    if form.mapping.iter().all(Option::is_none) {
        tab.error = Some("Associe pelo menos uma coluna do arquivo a uma coluna da tabela.".into());
        return;
    }
    let cols_of = |kind| {
        ts.cols
            .iter()
            .filter(|c| values::value_kind(Some(&c.data_type), std::iter::empty()) == kind)
            .map(|c| c.name.clone())
            .collect()
    };
    let (bool_cols, number_cols) = (cols_of(ValueKind::Bool), cols_of(ValueKind::Number));
    let spec = import::ImportSpec {
        path: path.into(),
        kind: s.cfg.kind,
        schema: ts.schema.clone(),
        table: ts.table.clone(),
        delimiter: pv.delimiter,
        has_header: form.has_header,
        mapping: form.mapping.clone(),
        bool_cols,
        number_cols,
        empty_is_null: form.empty_is_null,
    };
    tab.error = None;
    tab.pending += 1;
    s.send(Job::Import {
        tab: tab.id,
        spec: Box::new(spec),
    });
}

/// Adds `column = value` (or IS NULL) to a table tab's filter and reloads.
pub fn filter_by_value(tab: &mut Tab, s: &Session, column: &str, value: Option<&str>) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let cond = sql::filter_condition(s.cfg.kind, column, FilterOp::Eq, value);
    ts.filter = sql::and(&ts.filter, &cond);
    ts.page = 0;
    reload(tab, s);
}

/// Asks for the plan of one statement. ANALYZE runs it, so it is refused for anything but reads.
pub fn explain(tab: &mut Tab, s: &Session, sql: &str, analyze: bool) {
    let parts = sql::statement_ranges(sql, false);
    let [(a, b)] = parts[..] else {
        tab.error =
            Some("Explicar funciona com um comando por vez: coloque o cursor nele ou selecione-o.".into());
        return;
    };
    let one: String = sql.chars().skip(a).take(b - a).collect();
    if analyze && sql::script_risk(&one).writes {
        tab.error = Some(
            "EXPLAIN ANALYZE executa o comando de verdade: aqui ele só é usado em consultas de leitura."
                .into(),
        );
        return;
    }
    tab.error = None;
    tab.pending += 1;
    s.send(Job::Explain {
        tab: tab.id,
        sql: one,
        analyze,
    });
}

/// Carries out what the grid asked for. Returns text to put on the clipboard, if any.
pub fn handle_grid_event(tab: &mut Tab, s: &Session, event: GridEvent, editable: bool) -> Option<Effect> {
    let k = s.cfg.kind;
    let Some(res) = &tab.result else { return None };
    let no_cols = Vec::new();
    let cols = match &tab.kind {
        TabKind::Table(ts) => &ts.cols,
        TabKind::Sql { .. } | TabKind::Structure(_) | TabKind::Activity(_) | TabKind::Design(_) => &no_cols,
    };
    let kinds = values::column_kinds(res, cols);

    // Things that work on any result, editable or not.
    match &event {
        GridEvent::Select(row, col) => {
            tab.selected = Some((*row, *col));
            tab.anchor = None;
            tab.detail_hidden = None;
            if let TabKind::Table(ts) = &mut tab.kind {
                ts.bool_pick = None;
            }
            return None;
        }
        GridEvent::Copy(row, how) => {
            let table = match &tab.kind {
                TabKind::Table(ts) => k.qualified(&ts.schema, &ts.table),
                TabKind::Sql { .. } | TabKind::Structure(_) | TabKind::Activity(_) | TabKind::Design(_) => {
                    "tabela".to_string()
                }
            };
            return Some(Effect::Copy(match how {
                CopyAs::Json => values::row_json(res, cols, *row),
                CopyAs::Insert => sql::insert_row(k, &table, &res.columns, &res.rows[*row]),
            }));
        }
        GridEvent::Extend(row, col) => {
            tab.anchor = tab.anchor.or(tab.selected).or(Some((*row, *col)));
            tab.selected = Some((*row, *col));
            return None;
        }
        GridEvent::SelectAll => {
            let view = tab.view();
            if let (false, Some(&first), Some(&last)) = (res.rows.is_empty(), view.first(), view.last()) {
                tab.anchor = Some((0, first));
                tab.selected = Some((res.rows.len() - 1, last));
            }
            return None;
        }
        GridEvent::ToggleColumn(col) => {
            let name = res.columns[*col].clone();
            if let Some(i) = tab.hidden_cols.iter().position(|n| *n == name) {
                tab.hidden_cols.remove(i);
            } else {
                tab.hidden_cols.push(name);
                // A selection on a column that just went away would be invisible.
                if tab.selected.is_some_and(|(_, c)| c == *col) {
                    tab.selected = None;
                    tab.anchor = None;
                }
            }
            return None;
        }
        GridEvent::ShowAllColumns => {
            tab.hidden_cols.clear();
            return None;
        }
        GridEvent::TogglePin(col) => {
            let name = res.columns[*col].clone();
            if let Some(i) = tab.pinned_cols.iter().position(|n| *n == name) {
                tab.pinned_cols.remove(i);
            } else {
                tab.pinned_cols.push(name);
            }
            return None;
        }
        GridEvent::ColumnStats(col) => {
            let name = res.columns[*col].clone();
            let kind = kinds[*col];
            match &mut tab.kind {
                // Tables: the database counts every row that passes the filter.
                TabKind::Table(ts) => {
                    let filter = ts.effective_filter(k);
                    let ordered = matches!(kind, ValueKind::Number | ValueKind::Date);
                    let (summary, top) = sql::column_stats(k, &ts.schema, &ts.table, &name, &filter, ordered);
                    let scope = if filter.is_empty() {
                        "tabela inteira".to_string()
                    } else {
                        "linhas que passam no filtro".to_string()
                    };
                    tab.col_stats = Some(StatsView {
                        column: name,
                        scope,
                        data: None,
                    });
                    tab.pending += 1;
                    s.send(Job::ColumnStats {
                        tab: tab.id,
                        summary,
                        top,
                    });
                }
                // Editor results: only what was loaded.
                _ => {
                    let data = values::local_column_stats(res, *col, kind);
                    tab.col_stats = Some(StatsView {
                        column: name,
                        scope: format!("{} linha(s) carregadas", res.rows.len()),
                        data: Some(Ok(data)),
                    });
                }
            }
            return None;
        }
        GridEvent::CopyCells { headers } => {
            let range = tab.selection()?;
            // Copy what the grid shows, pending edits included.
            let overlay = match &tab.kind {
                TabKind::Table(ts) => ts.overlay(res),
                _ => None,
            };
            let shown = overlay.as_ref().map_or(res, |o| &o.result);
            return Some(Effect::Copy(values::to_tsv(shown, &range, *headers)));
        }
        GridEvent::FollowFk(row, col) => {
            let info = cols.iter().find(|c| c.name == res.columns[*col])?;
            let target = info.references.as_ref()?;
            let value = res.rows[*row][*col].as_deref()?;
            let filter = sql::filter_condition(k, &target.column, FilterOp::Eq, Some(value));
            return Some(Effect::OpenFiltered {
                schema: target.schema.clone(),
                table: target.table.clone(),
                filter,
            });
        }
        // Filtering doesn't change data, so read-only tables get it too.
        GridEvent::Filter(row, col, op) => {
            let cond = sql::filter_condition(k, &res.columns[*col], *op, res.rows[*row][*col].as_deref());
            if let TabKind::Table(ts) = &mut tab.kind {
                ts.filter = sql::and(&ts.filter, &cond);
                ts.page = 0;
                reload(tab, s);
            }
            return None;
        }
        _ => {}
    }

    let selection = tab.selection();
    let view = tab.view();
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
            if let Err(e) = set_cell(ts, res, &kinds, k, (row, col), val) {
                tab.error = Some(e);
            }
        }
        // Fills from the selection's top-left corner; whatever falls outside the page is left out.
        GridEvent::Paste(text) => {
            let sel = selection?;
            let top = sel.r0;
            // Columns go rightwards in on-screen order from the selection's first one.
            let start = view.iter().position(|&c| c == sel.cols[0]).unwrap_or(0);
            let cells = values::parse_tsv(&text);
            let mut skipped = 0;
            for (dr, line) in cells.iter().enumerate() {
                for (dc, v) in line.iter().enumerate() {
                    let row = top + dr;
                    let Some(&col) = view.get(start + dc).filter(|_| row < res.rows.len()) else {
                        skipped += 1;
                        continue;
                    };
                    // Spreadsheets have no NULL: the word itself stands for it, as the grid shows it.
                    let val = (v != "NULL").then(|| v.clone());
                    if let Err(e) = set_cell(ts, res, &kinds, k, (row, col), val) {
                        tab.error = Some(e);
                        return None;
                    }
                }
            }
            let rows = cells.len().min(res.rows.len().saturating_sub(top));
            let cols = cells
                .iter()
                .map(Vec::len)
                .max()
                .unwrap_or(0)
                .min(view.len().saturating_sub(start));
            if rows > 0 && cols > 0 {
                tab.anchor = Some((top, view[start]));
                tab.selected = Some((top + rows - 1, view[start + cols - 1]));
            }
            tab.error = (skipped > 0)
                .then(|| format!("{skipped} célula(s) coladas ficaram fora da página e foram ignoradas."));
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
        GridEvent::DuplicateRow(row) => {
            open_duplicate_form(tab, row);
        }
        GridEvent::Select(..)
        | GridEvent::ColumnStats(..)
        | GridEvent::ToggleColumn(..)
        | GridEvent::ShowAllColumns
        | GridEvent::TogglePin(..)
        | GridEvent::Extend(..)
        | GridEvent::SelectAll
        | GridEvent::CopyCells { .. }
        | GridEvent::Copy(..)
        | GridEvent::Filter(..)
        | GridEvent::FollowFk(..) => {}
    }
    None
}

/// Records a new value for a cell as a pending change (saved together with Ctrl+S). Typed
/// booleans ("true", "sim", "0"...) are normalized for the engine.
fn set_cell(
    ts: &mut TableState,
    res: &QueryResult,
    kinds: &[ValueKind],
    k: Kind,
    (row, col): (usize, usize),
    val: Option<String>,
) -> Result<(), String> {
    let val = match (kinds[col], val) {
        (ValueKind::Bool, Some(v)) => match sql::parse_bool(&v) {
            Some(b) => Some(sql::bool_value(k, b).to_string()),
            None => return Err(format!("\"{v}\" não é um booleano (use true ou false)")),
        },
        (_, v) => v,
    };
    let key = ts.row_key(res, row);
    let col_name = res.columns[col].clone();
    let before = res.rows[row][col].clone();
    ts.changes
        .retain(|c| !matches!(c, Change::Set { key: k2, col: c2, .. } if *k2 == key && *c2 == col_name));
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
    Ok(())
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
            txn: TxState::default(),
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
            references: None,
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
    fn paste_fills_cells_from_the_selection_and_copy_takes_the_range() {
        let (s, _jobs, _replies) = fake_session(Kind::Postgres);
        let mut tab = table_tab();
        if let Some(r) = &mut tab.result {
            r.rows
                .push(vec![Some("8".into()), Some("t".into()), Some("bia".into())]);
        }
        handle_grid_event(&mut tab, &s, GridEvent::Select(0, 1), true);
        // Two rows of (ativo, nome) from a spreadsheet; the third column overflows the page.
        let text = "sim\tcarla\textra\r\nNULL\tdani\r\n".to_string();
        handle_grid_event(&mut tab, &s, GridEvent::Paste(text), true);
        let TabKind::Table(ts) = &tab.kind else { panic!() };
        let sets: Vec<(String, Option<String>)> = ts
            .changes
            .iter()
            .map(|c| match c {
                Change::Set { col, value, .. } => (col.clone(), value.clone()),
                Change::Delete { .. } => panic!(),
            })
            .collect();
        assert_eq!(
            sets,
            [
                ("ativo".into(), Some("true".into())),
                ("nome".into(), Some("carla".into())),
                ("ativo".into(), None),
                ("nome".into(), Some("dani".into())),
            ]
        );
        assert!(tab.error.as_deref().is_some_and(|e| e.contains("1 célula")));
        assert_eq!(
            tab.range(),
            Some(CellRange {
                r0: 0,
                r1: 1,
                cols: vec![1, 2]
            }),
            "the pasted block ends up selected"
        );
        // Copy shows what the grid shows: the pasted (unsaved) values.
        let Some(Effect::Copy(tsv)) =
            handle_grid_event(&mut tab, &s, GridEvent::CopyCells { headers: true }, true)
        else {
            panic!()
        };
        assert_eq!(tsv, "ativo\tnome\ntrue\tcarla\n\tdani\n");
        handle_grid_event(&mut tab, &s, GridEvent::SelectAll, true);
        assert_eq!(
            tab.range(),
            Some(CellRange {
                r0: 0,
                r1: 1,
                cols: vec![0, 1, 2]
            })
        );
        // Hidden and pinned columns: ranges follow what's on screen.
        tab.hidden_cols = vec!["ativo".into()];
        tab.pinned_cols = vec!["nome".into()];
        assert_eq!(tab.view(), [2, 0]);
        handle_grid_event(&mut tab, &s, GridEvent::SelectAll, true);
        assert_eq!(
            tab.range(),
            Some(CellRange {
                r0: 0,
                r1: 1,
                cols: vec![2, 0]
            })
        );
        handle_grid_event(&mut tab, &s, GridEvent::Select(1, 1), true);
        assert_eq!(tab.range(), None);
    }

    #[test]
    fn read_only_tabs_ignore_edits_but_still_copy() {
        let (s, jobs, _replies) = fake_session(Kind::Postgres);
        let mut tab = table_tab();
        handle_grid_event(&mut tab, &s, GridEvent::Commit(0, 2, Some("x".into())), false);
        assert!(jobs.try_recv().is_err());
        let Some(Effect::Copy(json)) =
            handle_grid_event(&mut tab, &s, GridEvent::Copy(0, CopyAs::Json), false)
        else {
            panic!("expected a copy")
        };
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
