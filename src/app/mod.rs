//! Application state and the logic that acts on it. The `ui` modules draw this state and turn
//! clicks and keys into calls here; nothing in this file draws anything.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Instant;

use eframe::egui;

mod replies;

use crate::config;
use crate::db::{
    ActivityView, Canceller, ColumnInfo, ConnConfig, DbObject, FkEdge, IncomingFk, Kind, PreviewData,
    QueryResult, SchemaColumn, ServerInfo, TableRef, TableStructure,
};
use crate::design::Design;
use crate::diagram::DiagramState;
use crate::export::{self, Format};
use crate::favorites::{self, Favorite};
use crate::filters::{self, SavedFilter};
use crate::history::{self, Entry};
use crate::import;
use crate::plan::Plan;
use crate::search::SearchHit;
use crate::secrets::{Derived, SecretStore};
use crate::session::{self, SavedTab};
use crate::sql::{self, FilterOp, PageQuery, Stmt};
use crate::values::{self, CellRange, ValueKind};
use crate::worker::{self, ExportSource, ExportSpec, Job, Reply, TxState};

/// A message for the user, outside any tab (connection status, save problems), shown as a
/// toast: information goes away by itself, errors stay until closed.
pub struct Notice {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

impl Notice {
    pub fn info(text: impl Into<String>) -> Option<Self> {
        Some(Notice {
            text: text.into(),
            error: false,
            at: Instant::now(),
        })
    }

    pub fn error(text: impl Into<String>) -> Option<Self> {
        Some(Notice {
            text: text.into(),
            error: true,
            at: Instant::now(),
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
    /// The same columns with schema and type (schema search, hover info, ER diagram).
    pub schema_cols: Vec<SchemaColumn>,
    /// Foreign keys, for JOIN suggestions in the editor.
    pub fks: Vec<FkEdge>,
    /// Functions, procedures, triggers, views, sequences.
    pub objects: Vec<DbObject>,
    /// The server's databases, and where unqualified names go now.
    pub databases: Vec<String>,
    pub current_db: String,
    pub current_schema: String,
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
    /// Foreign keys in other tables that point here ("linhas que apontam para esta").
    pub incoming: Vec<IncomingFk>,
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
    /// "Filtrar valores" of a column, while open.
    pub value_filter: Option<ValueFilter>,
    /// The last `overlay`, and what it was built from: the grid asks for it every frame.
    overlay_cache: std::cell::RefCell<Option<(u64, std::rc::Rc<Overlay>)>>,
    /// Conditions "Filtrar valores" put in `filter`, so applying it again replaces them.
    pub value_conds: Vec<ValueCond>,
}

/// A column's values and how many rows have each.
pub type ValueCounts = Vec<(Option<String>, i64)>;

/// "Filtrar valores": a column's values to check and uncheck, like a spreadsheet filter.
pub struct ValueFilter {
    pub column: String,
    /// The column as compared (see `sql::value_expr`).
    pub expr: String,
    /// Values and row counts, most common first; `None` while loading.
    pub values: Option<Result<ValueCounts, String>>,
    /// Every value is listed (not cut at `sql::DISTINCT_LIMIT`).
    pub complete: bool,
    pub unchecked: HashSet<Option<String>>,
    /// Narrows the list as typed; Enter asks the database when the list isn't complete.
    pub search: String,
    pub focused: bool,
}

pub struct ValueCond {
    pub column: String,
    pub cond: String,
    pub unchecked: HashSet<Option<String>>,
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
#[derive(Clone, Debug, PartialEq, Hash)]
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
            incoming: vec![],
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
            value_filter: None,
            value_conds: Vec::new(),
            overlay_cache: Default::default(),
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
    /// Built again only when the changes or the result differ from last time.
    pub fn overlay(&self, res: &QueryResult) -> Option<std::rc::Rc<Overlay>> {
        use std::hash::{Hash, Hasher};
        if self.changes.is_empty() {
            return None;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.changes.hash(&mut h);
        // The result itself: a new one has its own rows (and timing).
        (
            res.rows.as_ptr() as usize,
            res.rows.len(),
            res.columns.len(),
            res.elapsed,
        )
            .hash(&mut h);
        let fingerprint = h.finish();
        if let Some((f, o)) = &*self.overlay_cache.borrow()
            && *f == fingerprint
        {
            return Some(o.clone());
        }
        let o = std::rc::Rc::new(self.build_overlay(res));
        *self.overlay_cache.borrow_mut() = Some((fingerprint, o.clone()));
        Some(o)
    }

    fn build_overlay(&self, res: &QueryResult) -> Overlay {
        let mut out = Overlay {
            result: res.clone(),
            changed: HashSet::new(),
            deleted: HashSet::new(),
        };
        // Changes by row key, and where the key columns are: one pass over the rows.
        let mut by_key: std::collections::HashMap<&[Option<String>], Vec<&Change>> = Default::default();
        for c in &self.changes {
            by_key.entry(c.key()).or_default().push(c);
        }
        let key_at: Vec<Option<usize>> = self
            .pk_names()
            .iter()
            .map(|p| res.columns.iter().position(|c| c == p))
            .collect();
        let mut key: Vec<Option<String>> = Vec::with_capacity(key_at.len());
        for row in 0..res.rows.len() {
            key.clear();
            key.extend(key_at.iter().map(|i| i.and_then(|i| res.rows[row][i].clone())));
            let Some(changes) = by_key.get(key.as_slice()) else {
                continue;
            };
            for c in changes {
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
        out
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
    /// SQL editor.
    Sql(Box<Editor>),
    Table(Box<TableState>),
    Structure(Box<StructState>),
    Activity(Box<ActivityState>),
    /// "Nova tabela" / "Editar estrutura".
    Design(Box<Design>),
    /// "Buscar valor em todas as tabelas".
    Search(Box<SearchState>),
    /// ER diagram of a schema.
    Diagram(Box<DiagramState>),
    /// Every query run, searchable.
    History(Box<HistoryState>),
    /// A `.sql` file run without opening it in an editor.
    Script(Box<ScriptState>),
    /// "Exportar .sql": tables (and a schema's objects) written as a script.
    Dump(Box<DumpState>),
    /// "Comparar estrutura" between two schemas or databases.
    Compare(Box<CompareState>),
}

pub struct CompareState {
    pub source_schema: String,
    /// Saved connection id of the target, its database and schema (MySQL: the database).
    pub target_conn: String,
    pub target_db: String,
    pub target_schema: String,
    pub running: bool,
    pub result: Option<Result<Vec<crate::compare::TableDiff>, String>>,
    /// What was compared, as shown with the result ("dev · public → homolog · public").
    pub compared: String,
    /// The SQL runs here (same connection and database), or must be run on the target.
    pub same_connection: bool,
    /// "Salvar .sql": the file written.
    pub saved: Option<PathBuf>,
}

/// What "Exportar .sql" covers.
pub enum DumpScope {
    Table(String, String),
    Schema(String),
    /// Every schema of the database in use (MySQL: that database).
    Database,
}

pub struct DumpState {
    /// For the title: "tabela public.clientes", "schema public", "banco loja".
    pub what: String,
    pub tables: Vec<(String, String)>,
    pub objects_of: Vec<String>,
    pub structure: bool,
    pub data: bool,
    pub drop: bool,
    pub running: bool,
    pub progress: crate::dump::DumpProgress,
    /// The file written, or why not.
    pub outcome: Option<Result<PathBuf, String>>,
    pub started: Option<Instant>,
    pub took: Option<std::time::Duration>,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

/// What an editor tab has besides the grid every tab shares.
pub struct Editor {
    pub text: String,
    /// EXPLAIN of an editor statement, shown in place of the result until closed.
    pub plan: Option<Plan>,
    /// Values asked for `:parameters` before running.
    pub params: Option<ParamPrompt>,
    /// Last value typed for each parameter name.
    pub param_values: std::collections::HashMap<String, String>,
    /// "Prévia" of an UPDATE/DELETE: rows it would touch.
    pub preview: Option<PreviewState>,
    /// The table a simple single-table SELECT read from, when its rows can be edited in place
    /// (columns and keys arrive with `Reply::Columns`).
    pub source: Option<Box<TableState>>,
    /// The SELECT behind `source`, re-run after saving.
    pub source_query: String,
    /// The `.sql` file being edited, and its text as last read or written (the tab has unsaved
    /// changes when the editor differs from it).
    pub file: Option<PathBuf>,
    pub saved_text: String,
    /// Closing was refused once because of unsaved file changes; the next close goes through.
    pub close_armed: bool,
    /// "Repetir a cada": seconds between automatic re-runs of `last_query`.
    pub repeat: Option<u64>,
    pub last_query: String,
    pub last_run: Option<Instant>,
    /// Where the last error is in `text` (chars), underlined until the text changes.
    pub error_at: Option<(usize, usize)>,
    /// Move the cursor to `error_at` on the next frame.
    pub error_jump: bool,
}

impl Editor {
    pub fn new(text: String) -> Self {
        Editor {
            text,
            plan: None,
            params: None,
            param_values: Default::default(),
            preview: None,
            source: None,
            source_query: String::new(),
            file: None,
            saved_text: String::new(),
            close_armed: false,
            repeat: None,
            last_query: String::new(),
            last_run: None,
            error_at: None,
            error_jump: false,
        }
    }
}

pub struct ScriptState {
    pub path: PathBuf,
    pub size: u64,
    /// All of it or nothing (one transaction).
    pub transaction: bool,
    pub running: bool,
    pub progress: crate::worker::ScriptProgress,
    /// Set once it ends: `None` when every statement ran.
    pub outcome: Option<Option<crate::worker::ScriptError>>,
    pub started: Option<Instant>,
    pub took: Option<std::time::Duration>,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
pub struct HistoryState {
    pub query: String,
    pub all_conns: bool,
    pub only_errors: bool,
    /// The entry shown in full: (connection, sql).
    pub selected: Option<(String, String)>,
    pub focus: bool,
}

pub struct SearchState {
    pub term: String,
    pub exact: bool,
    pub include_big: bool,
    pub running: bool,
    pub done: usize,
    pub total: usize,
    /// Big tables left out of the last search.
    pub skipped: usize,
    pub hits: Vec<SearchHit>,
    pub errors: Vec<String>,
    /// Set to stop the search between tables.
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    /// Focus the field on the first frame.
    pub focus: bool,
}

/// "Atividade" tab: sessions on the server, optionally refreshed every few seconds.
pub struct ActivityState {
    pub view: ActivityView,
    /// The "Servidor" view's data (`None` until it first arrives).
    pub server: Option<Result<ServerInfo, String>>,
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
    /// When the tab became busy, to tell the user about long ones finishing in the background.
    started: Option<Instant>,
    /// Selected cell `(row, column)` in the result grid.
    pub selected: Option<(usize, usize)>,
    /// Other corner of a range selection (Shift+click, drag, Shift+arrows); `selected` is the
    /// corner that moves.
    pub anchor: Option<(usize, usize)>,
    /// Row whose detail panel was closed with its X; the panel comes back on the next click.
    pub detail_hidden: Option<usize>,
    /// SQL run from the tab (editor, design), recorded in the history once its result arrives.
    pub pending_sql: Option<String>,
    /// SQL waiting for the user to confirm destructive statements.
    pub confirm_run: Option<String>,
    /// A cell's value open in the big editor.
    pub value_editor: Option<ValueEditor>,
    /// Last export of this tab: file and row count, shown until dismissed.
    pub exported: Option<(PathBuf, u64)>,
    /// A script's result sets when it returned more than one, in order. The one on screen lives
    /// in `result`; its slot here is an empty placeholder.
    pub sets: Vec<QueryResult>,
    pub set_shown: usize,
    /// "Definir valor na seleção": the value being typed.
    pub fill_prompt: Option<String>,
    pub col_stats: Option<StatsView>,
    /// Columns hidden from the grid, and columns pinned to its left edge (by name, so they
    /// survive reloads and paging).
    pub hidden_cols: Vec<String>,
    pub pinned_cols: Vec<String>,
    /// The user gave the tab its own name.
    pub renamed: bool,
}

/// "Estatísticas da coluna" dialog.
pub struct StatsView {
    pub column: String,
    /// What the numbers cover: the whole (filtered) table or just the loaded rows.
    pub scope: String,
    /// `None` while the database is still counting.
    pub data: Option<Result<values::ColumnStats, String>>,
}

/// "Prévia" of an UPDATE/DELETE: what it would touch, before deciding to run it.
pub struct PreviewState {
    pub sql: String,
    /// `None` while the database is counting.
    pub data: Option<Result<PreviewData, String>>,
}

/// What the editor does with a statement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RunMode {
    Run,
    /// Run without the automatic preview (the user just saw it).
    RunPreviewed,
    /// EXPLAIN, with ANALYZE when true.
    Explain(bool),
    Preview,
}

pub struct ParamPrompt {
    pub sql: String,
    pub names: Vec<String>,
    /// What to do once the values are in.
    pub mode: RunMode,
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
            started: None,
            selected: None,
            anchor: None,
            detail_hidden: None,
            pending_sql: None,
            confirm_run: None,
            value_editor: None,
            exported: None,
            sets: Vec::new(),
            set_shown: 0,
            fill_prompt: None,
            col_stats: None,
            hidden_cols: Vec::new(),
            pinned_cols: Vec::new(),
            renamed: false,
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

    pub fn editor(&self) -> Option<&Editor> {
        match &self.kind {
            TabKind::Sql(ed) => Some(ed),
            _ => None,
        }
    }

    pub fn editor_mut(&mut self) -> Option<&mut Editor> {
        match &mut self.kind {
            TabKind::Sql(ed) => Some(ed),
            _ => None,
        }
    }

    /// Sends a job that answers with `Reply::Rows`; the tab stays busy until it does.
    fn send_rows_job(&mut self, s: &Session, job: Job) {
        self.begin_job();
        s.send(job);
    }

    /// Counts a job sent for this tab; the first of a batch starts the clock.
    fn begin_job(&mut self) {
        if self.pending == 0 {
            self.started = Some(Instant::now());
        }
        self.pending += 1;
    }

    /// Counts a reply. Returns how long the tab was busy when this was the last one.
    pub(crate) fn end_job(&mut self) -> Option<std::time::Duration> {
        self.pending = self.pending.saturating_sub(1);
        if self.pending > 0 {
            return None;
        }
        self.started.take().map(|at| at.elapsed())
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

pub struct ConnTest {
    /// The settings tested (`Debug` of the config): a result says nothing once they change.
    pub key: String,
    pub rx: Receiver<Result<String, String>>,
    pub result: Option<Result<String, String>>,
}

/// A long value (JSON, text, XML…) in an editor of its own.
pub struct ValueEditor {
    pub row: usize,
    pub col: usize,
    pub column: String,
    pub data_type: String,
    pub text: String,
    /// The value as it came (`None`: NULL); saving the same thing changes nothing.
    pub original: Option<String>,
    /// Set NULL when saved (the text is kept in case it's unset again).
    pub null: bool,
    pub editable: bool,
    /// A `json`/`jsonb` column: only valid JSON is saved.
    pub json_column: bool,
    /// The value was JSON on one line: it goes back compact, only the layout shown changes.
    pub compact: bool,
}

impl ValueEditor {
    /// What saving writes: `None` for NULL, the JSON compacted again if it came compact.
    pub fn value(&self) -> Option<String> {
        if self.null {
            return None;
        }
        if self.compact && values::json_check(&self.text).is_ok() {
            return Some(values::json_compact(&self.text));
        }
        Some(self.text.clone())
    }
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
    /// The cell's value in the big editor (`row`, `col`).
    OpenValue(usize, usize),
    /// "Filtrar valores" of a column (table tabs).
    FilterValues(usize),
    /// Hide a shown column, or show a hidden one.
    ToggleColumn(usize),
    ShowAllColumns,
    /// Pin a column to the left edge, or unpin it.
    TogglePin(usize),
    /// Open the insert form filled with this row.
    DuplicateRow(usize),
    /// Open the rows of another table whose foreign key (`incoming[i]`) points at this row.
    FollowIncoming(usize, usize),
    /// Copy the selection (or, without one, the whole result) as a Markdown table.
    CopyMarkdown,
    /// Ask for a value to put in every selected cell.
    AskFill,
    /// Every selected cell gets this value (`None`: NULL).
    FillSelection(Option<String>),
    /// Each selected column takes the value of the selection's first row (Ctrl+D).
    FillDown,
    /// Mark the selected rows for deletion.
    DeleteRows,
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
    /// "Testar conexão" in the form: running, or its outcome for the settings it was run with.
    pub conn_test: Option<ConnTest>,
    pub palette: Option<Palette>,
    pub tabs: Vec<Tab>,
    pub active: usize,
    next_id: u64,
    pub filter: String,
    /// The schema (MySQL: database) just switched to: the sidebar opens it and folds the others
    /// once.
    pub schema_focus: Option<String>,
    pub notice: Option<Notice>,
    /// Redraw every frame at the monitor's rate (for the FPS readout). Off by default: an idle
    /// app should use no CPU.
    pub max_fps: bool,
    /// The FPS readout in the corner (Ctrl+P), off unless asked for.
    pub show_fps: bool,
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
    /// Recently closed tabs, newest last.
    pub closed: Vec<SavedTab>,
    pub path_prompt: Option<PathPrompt>,
    pub settings: crate::settings::Settings,
    pub table_action: Option<TableActionPrompt>,
    /// The window title announces a finished job (cleared when the window is focused).
    title_changed: bool,
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
            conn_test: None,
            palette: None,
            tabs: vec![],
            active: 0,
            next_id: 1,
            filter: String::new(),
            schema_focus: None,
            notice: None,
            max_fps: false,
            show_fps: false,
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
            closed: Vec::new(),
            path_prompt: None,
            settings: crate::settings::load(),
            table_action: None,
            title_changed: false,
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
        let tabs = self.tabs.iter().filter_map(saved_tab).collect();
        session::Saved {
            conn: self.session.as_ref().map(|s| s.cfg.id.clone()),
            tabs,
            active: self.active,
            window: self.window,
        }
    }

    fn restore_tabs(&mut self, saved: session::Saved) {
        for t in saved.tabs {
            self.restore_tab(t);
        }
        self.active = saved.active.min(self.tabs.len().saturating_sub(1));
    }

    /// Opens a tab as it was saved (session restore, "reabrir aba fechada").
    fn restore_tab(&mut self, t: SavedTab) {
        {
            match t {
                SavedTab::Sql { text, title, file } => {
                    self.open_sql(text);
                    let Some(tab) = self.tabs.last_mut() else { return };
                    if let Some(title) = title {
                        tab.title = title;
                        tab.renamed = true;
                    }
                    // The editor keeps the text it had (unsaved edits included); the file on disk
                    // says what "saved" means.
                    if let Some(path) = file {
                        tab.title = crate::files::title(&path);
                        if let Some(ed) = tab.editor_mut() {
                            ed.saved_text = crate::files::read_sql(&path).unwrap_or_default();
                            ed.file = Some(path);
                        }
                    }
                }
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
    }

    /// Opens a `.sql` file in a new editor tab (or focuses the tab that has it).
    pub fn open_file(&mut self, path: PathBuf) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.editor().is_some_and(|ed| ed.file.as_ref() == Some(&path)))
        {
            self.active = i;
            return;
        }
        match crate::files::read_sql(&path) {
            Ok(text) => {
                self.open_sql(text.clone());
                if let Some(t) = self.tabs.last_mut() {
                    t.title = crate::files::title(&path);
                    if let Some(ed) = t.editor_mut() {
                        ed.saved_text = text;
                        ed.file = Some(path);
                    }
                }
            }
            Err(e) => self.notice = Notice::error(e),
        }
    }

    /// Writes an editor tab to `path` (its file from now on).
    pub fn save_file_as(&mut self, i: usize, path: PathBuf) {
        let Some(t) = self.tabs.get_mut(i) else { return };
        let TabKind::Sql(ed) = &mut t.kind else { return };
        match crate::files::write_sql(&path, &ed.text) {
            Ok(written) => {
                ed.saved_text = ed.text.clone();
                t.title = crate::files::title(&written);
                self.notice = Notice::info(format!("Salvo em {}", written.display()));
                ed.file = Some(written);
                ed.close_armed = false;
            }
            Err(e) => self.notice = Notice::error(e),
        }
    }

    /// Ctrl+S on an editor tab: its file, or "salvar como" when it has none yet.
    pub fn save_file(&mut self, i: usize, ask: bool) {
        let Some(t) = self.tabs.get(i) else { return };
        match (t.editor().and_then(|ed| ed.file.as_ref()), ask) {
            (Some(path), false) => {
                let path = path.clone();
                self.save_file_as(i, path);
            }
            _ => {
                let suggested = format!("{}.sql", t.title.trim_end_matches(".sql"));
                if crate::files::NATIVE_DIALOGS {
                    if let Some(path) = crate::files::pick_save(&suggested) {
                        self.save_file_as(i, path);
                    }
                } else {
                    self.path_prompt = Some(PathPrompt {
                        save_tab: Some(t.id),
                        path: suggested,
                        run: false,
                    });
                }
            }
        }
    }

    /// Ctrl+O.
    pub fn open_file_dialog(&mut self) {
        if crate::files::NATIVE_DIALOGS {
            if let Some(path) = crate::files::pick_open() {
                self.open_file(path);
            }
        } else {
            self.path_prompt = Some(PathPrompt {
                save_tab: None,
                path: String::new(),
                run: false,
            });
        }
    }

    /// Ctrl+Shift+T: the most recently closed tab comes back.
    pub fn reopen_closed_tab(&mut self) {
        if self.session.as_ref().is_some_and(|s| s.ready)
            && let Some(t) = self.closed.pop()
        {
            self.restore_tab(t);
        }
    }

    /// A copy of an editor tab, next to it.
    pub fn duplicate_tab(&mut self, i: usize) {
        let Some(t) = self.tabs.get(i) else { return };
        let Some(ed) = t.editor() else { return };
        let (text, title) = (ed.text.clone(), format!("{} (cópia)", t.title));
        self.open_sql(text);
        if let Some(t) = self.tabs.last_mut() {
            t.title = title;
            t.renamed = true;
        }
    }

    /// Closes every tab but `keep` (tabs with unsaved changes stay, with their warning).
    pub fn close_others(&mut self, keep: usize, only_right: bool) {
        let ids: Vec<u64> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != keep && (!only_right || *i > keep))
            .map(|(_, t)| t.id)
            .collect();
        for id in ids {
            if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                self.close_tab(i);
            }
        }
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| self.tabs.get(keep).is_some_and(|k| k.id == t.id))
        {
            self.active = i;
        }
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
            Some(i) => self.saved[i] = cfg.clone(),
            None => {
                cfg.id = config::new_id();
                self.saved.push(cfg.clone());
            }
        }
        self.save_connections();
        // Editing the open connection: what can change live does, now.
        if let Some(s) = &mut self.session
            && s.cfg.id == cfg.id
            && !apply_live_settings(s, &cfg)
        {
            self.notice = Notice::info(
                "Salvo. Reconecte (menu … → Reconectar) para aplicar servidor, usuário, banco, TLS, \
                 somente leitura, timeout ou SSH.",
            );
        }
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
        self.connect_cfg(cfg);
    }

    /// MySQL: `USE` it. Postgres schemas: first in the search path. Another Postgres database
    /// needs a new connection (same server and credentials); its tabs close like a switch of
    /// connection.
    pub fn switch_location(&mut self, database: Option<&str>, schema: Option<&str>) {
        let Some(s) = &self.session else { return };
        match (s.cfg.kind, database, schema) {
            (Kind::MySql, Some(db), _) => s.send(Job::UseSchema(db.to_string())),
            (Kind::Postgres, _, Some(sc)) => s.send(Job::UseSchema(sc.to_string())),
            (Kind::Postgres, Some(db), None) if db != s.current_db => {
                let cfg = ConnConfig {
                    database: db.to_string(),
                    ..s.cfg.clone()
                };
                self.connect_cfg(cfg);
            }
            _ => {}
        }
    }

    fn connect_cfg(&mut self, cfg: ConnConfig) {
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
            schema_cols: vec![],
            fks: vec![],
            objects: vec![],
            databases: vec![],
            current_db: String::new(),
            current_schema: String::new(),
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

    /// Something slow finished while the window was in the background: flash the taskbar
    /// button and say so in the title until the window is focused again.
    fn notify_finished(&mut self, finished: &[(String, std::time::Duration)]) {
        let Some((title, took)) = finished
            .iter()
            .filter(|(_, d)| *d >= LONG_JOB)
            .max_by_key(|(_, d)| *d)
        else {
            return;
        };
        if self.ctx.input(|i| i.viewport().focused) != Some(false) {
            return;
        }
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "✓ {title} terminou ({} s) · dblite",
            took.as_secs()
        )));
        self.title_changed = true;
    }

    /// Back in the window: the plain title again.
    pub fn clear_finished_notice(&mut self) {
        if self.title_changed && self.ctx.input(|i| i.viewport().focused) == Some(true) {
            self.title_changed = false;
            self.ctx
                .send_viewport_cmd(egui::ViewportCommand::Title("dblite".into()));
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
                    t.begin_job();
                    s.send(Job::Structure {
                        tab: t.id,
                        schema: schema.into(),
                        table: table.into(),
                    });
                }
                TabKind::Table(ts) if ts.schema == schema && ts.table == table && ts.changes.is_empty() => {
                    // New columns: their info first, then the page (SELECT * picks them up).
                    t.begin_job();
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
            && file_dirty(t)
            && let Some(ed) = t.editor_mut()
            && !ed.close_armed
        {
            ed.close_armed = true;
            t.error = Some(format!(
                "{} tem alterações não salvas: Ctrl+S salva; feche de novo para descartar.",
                t.title
            ));
            self.active = i;
            return;
        }
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
            let t = self.tabs.remove(i);
            if let Some(saved) = saved_tab(&t) {
                self.closed.push(saved);
                if self.closed.len() > MAX_CLOSED {
                    self.closed.remove(0);
                }
            }
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
        }
    }

    pub fn open_sql(&mut self, text: String) {
        let n = self
            .tabs
            .iter()
            .filter(|t| matches!(t.kind, TabKind::Sql(_)))
            .count()
            + 1;
        self.push_tab(format!("SQL {n}"), TabKind::Sql(Box::new(Editor::new(text))));
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

    /// Opens (or focuses) the ER diagram of `schema` (the connection's first one when `None`).
    pub fn open_diagram(&mut self, schema: Option<&str>) {
        let Some(s) = &self.session else { return };
        let schema = schema
            .map(str::to_string)
            .or_else(|| s.tables.first().map(|t| t.schema.clone()))
            .unwrap_or_default();
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| matches!(&t.kind, TabKind::Diagram(d) if d.schema == schema))
        {
            self.active = i;
            return;
        }
        let i = self.push_tab(
            format!("diagrama · {schema}"),
            TabKind::Diagram(Box::new(DiagramState::new(&schema))),
        );
        let Some(s) = &self.session else { return };
        let tab = &mut self.tabs[i];
        tab.begin_job();
        s.send(Job::Fks { tab: tab.id });
    }

    /// The code of a function, trigger, view or sequence, in a new editor tab.
    pub fn open_object(&mut self, object: DbObject) {
        if self.session.is_none() {
            return;
        }
        self.open_sql(format!("-- carregando a definição de {}…", object.name));
        let Some(s) = &self.session else { return };
        let Some(tab) = self.tabs.last_mut() else { return };
        tab.title = object.name.clone();
        tab.renamed = true;
        tab.begin_job();
        s.send(Job::Source { tab: tab.id, object });
    }

    /// Runs the confirmed table action. Tabs showing a renamed or dropped table close (their
    /// queries would fail); an emptied one reloads.
    pub fn run_table_action(&mut self, p: TableActionPrompt) {
        let Some(s) = &self.session else { return };
        let sql = sql::table_action(s.cfg.kind, &p.schema, &p.table, p.action, &p.input);
        let done = match p.action {
            sql::TableAction::Rename => format!("{} renomeada para {}", p.table, p.input.trim()),
            sql::TableAction::Duplicate(_) => format!("{} duplicada em {}", p.table, p.input.trim()),
            sql::TableAction::Truncate => format!("{} esvaziada", p.table),
            sql::TableAction::Drop => format!("{} excluída", p.table),
        };
        s.send(Job::Admin { sql, done });
        let same = |t: &Tab| match &t.kind {
            TabKind::Table(ts) => ts.schema == p.schema && ts.table == p.table,
            TabKind::Structure(st) => st.schema == p.schema && st.table == p.table,
            _ => false,
        };
        match p.action {
            sql::TableAction::Rename | sql::TableAction::Drop => {
                self.tabs.retain(|t| !same(t));
                self.active = self.active.min(self.tabs.len().saturating_sub(1));
            }
            sql::TableAction::Truncate => {
                let Some(s) = &self.session else { return };
                for t in self.tabs.iter_mut().filter(|t| same(t)) {
                    reload(t, s);
                }
            }
            sql::TableAction::Duplicate(_) => {}
        }
    }

    /// "Executar…" on a function or procedure: a new editor tab with the call, asking for the
    /// arguments first (when there are any).
    pub fn run_object(&mut self, object: &DbObject) {
        let Some(s) = &self.session else { return };
        let args = sql::parse_args(&object.args);
        let procedure = object.kind == crate::db::ObjKind::Procedure;
        let call = sql::call_template(s.cfg.kind, &object.schema, &object.name, &args, procedure);
        self.open_sql(call.clone());
        let Some(s) = &self.session else { return };
        let Some(tab) = self.tabs.last_mut() else { return };
        tab.title = format!("{} ()", object.name);
        tab.renamed = true;
        let names = sql::param_names(&call);
        if names.is_empty() {
            run_sql(tab, s, call);
        } else if let Some(ed) = tab.editor_mut() {
            ed.params = Some(ParamPrompt {
                sql: call,
                names,
                mode: RunMode::Run,
                focused: false,
            });
        }
    }

    /// "Gerar SQL": a statement for a table in a new editor tab.
    pub fn open_template(&mut self, schema: &str, table: &str, what: sql::Template) {
        let Some(s) = &self.session else { return };
        let cols: Vec<(String, bool)> = s
            .schema_cols
            .iter()
            .filter(|c| c.schema == schema && c.table == table)
            .map(|c| (c.column.clone(), c.pk))
            .collect();
        let text = sql::template(s.cfg.kind, schema, table, &cols, what);
        self.open_sql(text);
    }

    /// A `.sql` file in a "executar arquivo" tab (not run yet: the tab has the button).
    pub fn open_script(&mut self, path: PathBuf) {
        let Some(s) = &self.session else { return };
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let st = ScriptState {
            path: path.clone(),
            size,
            // Postgres undoes DDL too; MySQL commits on each DDL, so a transaction can't
            // promise "all or nothing" there.
            transaction: s.cfg.kind == Kind::Postgres,
            running: false,
            progress: Default::default(),
            outcome: None,
            started: None,
            took: None,
            cancel: Arc::default(),
        };
        self.push_tab(crate::files::title(&path), TabKind::Script(Box::new(st)));
    }

    /// "Exportar .sql" of a table, a schema or the whole database, in a tab with its options.
    pub fn open_dump(&mut self, scope: DumpScope) {
        let Some(s) = &self.session else { return };
        let all = |schema: Option<&str>| -> Vec<(String, String)> {
            s.tables
                .iter()
                .filter(|t| !t.is_view && schema.is_none_or(|sc| t.schema == sc))
                .map(|t| (t.schema.clone(), t.name.clone()))
                .collect()
        };
        let (what, tables, objects_of) = match scope {
            DumpScope::Table(schema, table) => (
                format!("tabela {schema}.{table}"),
                vec![(schema, table)],
                Vec::new(),
            ),
            DumpScope::Schema(schema) => (format!("schema {schema}"), all(Some(&schema)), vec![schema]),
            // MySQL lists every database in the sidebar; "the database" is the one in use.
            DumpScope::Database if s.cfg.kind == Kind::MySql => (
                format!("banco {}", s.current_db),
                all(Some(&s.current_db)),
                vec![s.current_db.clone()],
            ),
            DumpScope::Database => {
                let tables = all(None);
                let mut schemas: Vec<String> = tables.iter().map(|(sc, _)| sc.clone()).collect();
                schemas.dedup();
                (format!("banco {}", s.current_db), tables, schemas)
            }
        };
        let title = format!(
            "Exportar {}",
            what.split_once(' ').map_or(what.as_str(), |(_, w)| w)
        );
        let st = DumpState {
            what,
            tables,
            objects_of,
            structure: true,
            data: true,
            drop: false,
            running: false,
            progress: Default::default(),
            outcome: None,
            started: None,
            took: None,
            cancel: Arc::default(),
        };
        self.push_tab(title, TabKind::Dump(Box::new(st)));
    }

    /// "Comparar estrutura": this connection's schema against another (here or elsewhere).
    pub fn open_compare(&mut self, schema: Option<String>) {
        let Some(s) = &self.session else { return };
        let here = match s.cfg.kind {
            Kind::Postgres => s.current_schema.clone(),
            Kind::MySql => s.current_db.clone(),
        };
        let st = CompareState {
            source_schema: schema.unwrap_or(here),
            target_conn: s.cfg.id.clone(),
            target_db: s.current_db.clone(),
            target_schema: String::new(),
            running: false,
            result: None,
            compared: String::new(),
            same_connection: true,
            saved: None,
        };
        self.push_tab("Comparar estrutura".into(), TabKind::Compare(Box::new(st)));
    }

    /// Ctrl+P "Executar arquivo .sql…".
    pub fn pick_script(&mut self) {
        if crate::files::NATIVE_DIALOGS {
            if let Some(path) = crate::files::pick_open() {
                self.open_script(path);
            }
        } else {
            self.path_prompt = Some(PathPrompt {
                save_tab: None,
                path: String::new(),
                run: true,
            });
        }
    }

    /// Opens (or focuses) the full query history.
    pub fn open_history(&mut self) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| matches!(t.kind, TabKind::History(_)))
        {
            self.active = i;
            return;
        }
        let st = HistoryState {
            focus: true,
            ..Default::default()
        };
        self.push_tab("Histórico".into(), TabKind::History(Box::new(st)));
    }

    /// Drops one entry from the history (and the file).
    pub fn forget_history(&mut self, conn: &str, sql: &str) {
        history::forget(&mut self.history, conn, sql);
    }

    /// Opens the "buscar valor" tab (or focuses it).
    pub fn open_search(&mut self) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| matches!(t.kind, TabKind::Search(_)))
        {
            self.active = i;
            return;
        }
        if self.session.is_none() {
            return;
        }
        let st = SearchState {
            term: String::new(),
            exact: false,
            include_big: false,
            running: false,
            done: 0,
            total: 0,
            skipped: 0,
            hits: Vec::new(),
            errors: Vec::new(),
            cancel: Arc::default(),
            focus: true,
        };
        self.push_tab("Buscar valor".into(), TabKind::Search(Box::new(st)));
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
            view: ActivityView::Sessions,
            server: None,
            auto_refresh: true,
            last_refresh: None,
            confirm_kill: None,
            kill_error: None,
        };
        let i = self.push_tab("Atividade".into(), TabKind::Activity(Box::new(state)));
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
        tab.begin_job();
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
        tab.begin_job();
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
    tab.begin_job();
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
    let (id, view) = (tab.id, st.view);
    if view == ActivityView::Server {
        tab.begin_job();
        s.send(Job::ServerInfo { tab: id });
        return;
    }
    tab.send_rows_job(s, Job::Activity { tab: id, view });
}

/// Starts "Comparar estrutura" with what the tab has chosen.
pub fn start_compare(tab: &mut Tab, s: &Session, saved: &[ConnConfig]) {
    let TabKind::Compare(st) = &mut tab.kind else {
        return;
    };
    if st.running || st.target_schema.trim().is_empty() {
        return;
    }
    let Some(cfg) = saved
        .iter()
        .find(|c| c.id == st.target_conn)
        .or(Some(&s.cfg).filter(|c| c.id == st.target_conn))
    else {
        return;
    };
    let target_db = st.target_db.trim().to_string();
    // The same connection compares in place: MySQL sees every database, Postgres only its own.
    let same = cfg.id == s.cfg.id && (s.cfg.kind == Kind::MySql || target_db == s.current_db);
    let target = (!same).then(|| {
        Box::new(ConnConfig {
            database: if target_db.is_empty() {
                cfg.database.clone()
            } else {
                target_db.clone()
            },
            ..cfg.clone()
        })
    });
    let schema_note = |conn: &str, db: &str, schema: &str| match s.cfg.kind {
        Kind::Postgres => format!("{conn} · {db} · {schema}"),
        Kind::MySql => format!("{conn} · {schema}"),
    };
    st.compared = format!(
        "{} → {}",
        schema_note(&s.cfg.name, &s.current_db, &st.source_schema),
        schema_note(&cfg.name, &target_db, st.target_schema.trim())
    );
    st.same_connection = same;
    st.running = true;
    st.result = None;
    st.saved = None;
    let job = Job::Compare {
        tab: tab.id,
        source_schema: st.source_schema.clone(),
        target,
        target_schema: st.target_schema.trim().to_string(),
    };
    tab.begin_job();
    s.send(job);
}

/// The SQL of a comparison, with where it must run.
pub fn compare_script(st: &CompareState) -> String {
    let Some(Ok(diffs)) = &st.result else {
        return String::new();
    };
    format!(
        "-- Comparação de estrutura: {}\n-- Execute no DESTINO. Remoção de tabelas fica comentada.\n{}\n",
        st.compared,
        crate::compare::script(diffs)
    )
}

/// Starts "Exportar .sql": a new file in Downloads named after what is exported.
pub fn start_dump(tab: &mut Tab, s: &Session) {
    let TabKind::Dump(st) = &mut tab.kind else { return };
    if st.running || (!st.structure && !st.data) {
        return;
    }
    let base = st.what.split_once(' ').map_or(st.what.as_str(), |(_, w)| w);
    let spec = crate::dump::DumpSpec {
        path: export::target_path(base, Format::Sql),
        tables: st.tables.clone(),
        objects_of: st.objects_of.clone(),
        structure: st.structure,
        data: st.data,
        drop: st.drop,
    };
    st.cancel = Arc::default();
    st.running = true;
    st.outcome = None;
    st.progress = Default::default();
    st.started = Some(Instant::now());
    st.took = None;
    let job = Job::Dump {
        tab: tab.id,
        spec,
        cancel: st.cancel.clone(),
    };
    tab.begin_job();
    s.send(job);
}

pub fn cancel_dump(tab: &mut Tab, s: &Session) {
    if let TabKind::Dump(st) = &mut tab.kind
        && st.running
    {
        st.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        s.cancel();
    }
}

/// Runs the tab's script file.
pub fn start_script(tab: &mut Tab, s: &Session) {
    let TabKind::Script(st) = &mut tab.kind else {
        return;
    };
    if st.running {
        return;
    }
    st.cancel = Arc::default();
    st.running = true;
    st.outcome = None;
    st.progress = Default::default();
    st.started = Some(Instant::now());
    st.took = None;
    let job = Job::RunScript {
        tab: tab.id,
        path: st.path.clone(),
        transaction: st.transaction,
        cancel: st.cancel.clone(),
    };
    tab.begin_job();
    s.send(job);
}

/// Stops the script: after the current statement, or right away by cancelling it.
pub fn cancel_script(tab: &mut Tab, s: &Session) {
    if let TabKind::Script(st) = &mut tab.kind
        && st.running
    {
        st.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        s.cancel();
    }
}

/// Starts looking for the tab's term in every table.
pub fn start_search(tab: &mut Tab, s: &Session) {
    let TabKind::Search(st) = &mut tab.kind else {
        return;
    };
    let term = st.term.trim().to_string();
    if term.is_empty() || st.running {
        return;
    }
    let (targets, skipped) = crate::search::targets(&s.tables, &s.schema_cols, &term, st.include_big);
    st.cancel = Arc::default();
    st.running = !targets.is_empty();
    st.done = 0;
    st.total = targets.len();
    st.skipped = skipped;
    st.hits.clear();
    st.errors.clear();
    if targets.is_empty() {
        return;
    }
    let job = Job::SearchValue {
        tab: tab.id,
        term,
        exact: st.exact,
        targets,
        cancel: st.cancel.clone(),
    };
    tab.begin_job();
    s.send(job);
}

/// Stops the search: after the current table, or right away by cancelling its query.
pub fn cancel_search(tab: &mut Tab, s: &Session) {
    if let TabKind::Search(st) = &mut tab.kind
        && st.running
    {
        st.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        s.cancel();
    }
}

/// Zeroes the "consultas lentas" counters on the server.
pub fn reset_query_stats(tab: &mut Tab, s: &Session) {
    let TabKind::Activity(st) = &mut tab.kind else {
        return;
    };
    st.last_refresh = Some(Instant::now());
    let id = tab.id;
    tab.send_rows_job(s, Job::ResetQueryStats { tab: id });
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
    let (tab_id, view) = (tab.id, st.view);
    tab.send_rows_job(
        s,
        Job::Kill {
            tab: tab_id,
            id,
            terminate,
            view,
        },
    );
}

/// The sidebar's table for a name from a query: exact first, then ignoring case (unquoted
/// names fold in Postgres). Without a schema, the name must be unambiguous.
fn resolve_table(s: &Session, schema: Option<&str>, table: &str) -> Option<(String, String)> {
    let pick = |exact: bool| {
        let same = |a: &str, b: &str| if exact { a == b } else { a.eq_ignore_ascii_case(b) };
        let found: Vec<&TableRef> = s
            .tables
            .iter()
            .filter(|t| !t.is_view && same(&t.name, table) && schema.is_none_or(|sc| same(&t.schema, sc)))
            .collect();
        match found[..] {
            [one] => Some((one.schema.clone(), one.name.clone())),
            // MySQL: the connection's database wins among same-named tables.
            _ => found
                .iter()
                .find(|t| schema.is_none() && t.schema == s.current_db)
                .map(|t| (t.schema.clone(), t.name.clone())),
        }
    };
    pick(true).or_else(|| pick(false))
}

/// The dialog of a table action: the new name, or the table's name typed to confirm.
pub struct TableActionPrompt {
    pub schema: String,
    pub table: String,
    pub action: sql::TableAction,
    pub input: String,
}

/// Where there are no native file dialogs: the path, typed.
pub struct PathPrompt {
    /// `Some(tab id)` to save that tab; `None` to open a file.
    pub save_tab: Option<u64>,
    pub path: String,
    /// Open it in an "executar arquivo" tab instead of the editor.
    pub run: bool,
}

/// An editor tab whose text differs from its file.
pub fn file_dirty(t: &Tab) -> bool {
    t.editor()
        .is_some_and(|ed| ed.file.is_some() && ed.text != ed.saved_text)
}

/// Closed tabs remembered for "reabrir aba fechada".
const MAX_CLOSED: usize = 20;

/// A tab as the session file (and the closed-tabs list) keeps it; `None` for tabs that are
/// only meaningful while open.
fn saved_tab(t: &Tab) -> Option<SavedTab> {
    match &t.kind {
        TabKind::Activity(_)
        | TabKind::Design(_)
        | TabKind::Search(_)
        | TabKind::Diagram(_)
        | TabKind::History(_)
        | TabKind::Script(_)
        | TabKind::Dump(_)
        | TabKind::Compare(_) => None,
        TabKind::Sql(ed) => Some(SavedTab::Sql {
            text: ed.text.clone(),
            title: (t.renamed && ed.file.is_none()).then(|| t.title.clone()),
            file: ed.file.clone(),
        }),
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
    }
}

/// Jobs at least this long announce themselves when they finish in the background.
const LONG_JOB: std::time::Duration = std::time::Duration::from_secs(10);

/// Copies the settings that don't need a new connection (name, color, preview) from an edited
/// copy of the open connection. Returns false when other changes wait for a reconnect.
pub fn apply_live_settings(s: &mut Session, edited: &ConnConfig) -> bool {
    s.cfg.name = edited.name.clone();
    s.cfg.color = edited.color;
    s.cfg.preview_writes = edited.preview_writes;
    // The saved default; the session's own mode is switched in the "…" menu.
    s.cfg.manual_commit = edited.manual_commit;
    s.cfg.same_session_settings(edited)
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
    if let Some(ed) = tab.editor_mut() {
        if let Some(src) = &ed.source
            && !src.changes.is_empty()
        {
            let n = src.changes.len();
            tab.error = Some(format!(
                "Salve (Ctrl+S) ou descarte as {n} alteração(ões) pendentes no resultado antes de executar outra consulta."
            ));
            return;
        }
        ed.source = None;
        ed.plan = None;
        ed.error_at = None;
        ed.last_query = sql.clone();
        ed.last_run = Some(Instant::now());
    }
    tab.pending_sql = Some(sql.clone());
    tab.error = None;
    let id = tab.id;
    tab.send_rows_job(s, Job::Query { tab: id, sql });
}

/// Where an error is in the editor.
#[derive(Debug, PartialEq)]
pub struct ErrorSpot {
    /// Char range in the editor text: the word the server complains about.
    pub start: usize,
    pub end: usize,
    /// 1-based, for the message.
    pub line: usize,
    pub column: usize,
}

/// Finds the error of `sent` in the editor `text`, when the editor still has that SQL (a run
/// with `:parameters` sent other text, so it has no place there).
pub fn locate_error(text: &str, sent: &str, e: &crate::error::DbError) -> Option<ErrorSpot> {
    let at = text.find(sent)? + e.position_in(sent)?;
    let ident = |c: char| c.is_alphanumeric() || matches!(c, '_' | '"' | '`' | '.');
    let rest = &text[at..];
    let len = match rest.chars().next()? {
        c if ident(c) => rest.find(|c: char| !ident(c)).unwrap_or(rest.len()),
        c => c.len_utf8(),
    };
    let before = &text[..at];
    let start = before.chars().count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    Some(ErrorSpot {
        start,
        end: start + text[at..at + len].chars().count(),
        line: before.matches('\n').count() + 1,
        column: text[line_start..at].chars().count() + 1,
    })
}

/// Opens "Filtrar valores" for a column of a table tab, with what was unchecked last time.
pub fn open_value_filter(tab: &mut Tab, s: &Session, column: &str) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let data_type = ts
        .cols
        .iter()
        .find(|c| c.name == column)
        .map_or("", |c| c.data_type.as_str());
    let unchecked = ts
        .value_conds
        .iter()
        .find(|v| v.column == column)
        .map(|v| v.unchecked.clone())
        .unwrap_or_default();
    ts.value_filter = Some(ValueFilter {
        column: column.to_string(),
        expr: sql::value_expr(s.cfg.kind, column, data_type),
        values: None,
        complete: false,
        unchecked,
        search: String::new(),
        focused: false,
    });
    load_values(tab, s);
}

/// Asks for the open filter's values: the rows the other conditions let through (its own
/// condition left out, so what it hides can be checked again).
pub fn load_values(tab: &mut Tab, s: &Session) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let Some(vf) = &ts.value_filter else { return };
    let k = s.cfg.kind;
    let mut filter = ts.filter.clone();
    if let Some(own) = ts.value_conds.iter().find(|v| v.column == vf.column)
        && let Some(rest) = sql::remove_condition(&filter, &own.cond)
    {
        filter = rest;
    }
    let names: Vec<String> = ts.cols.iter().map(|c| c.name.clone()).collect();
    let filter = sql::and(&filter, &sql::search_condition(k, &names, &ts.search));
    let sql = sql::distinct_values(k, &ts.schema, &ts.table, &vf.expr, &filter, &vf.search);
    if let Some(vf) = &mut ts.value_filter {
        vf.values = None;
    }
    tab.begin_job();
    s.send(Job::Distinct { tab: tab.id, sql });
}

/// Puts the checked values in the table's filter (replacing what this column had) and reloads.
pub fn apply_value_filter(tab: &mut Tab, s: &Session) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let Some(vf) = ts.value_filter.take() else { return };
    let Some(Ok(values)) = &vf.values else { return };
    let (unchecked, checked): (Vec<_>, Vec<_>) = values
        .iter()
        .map(|(v, _)| v.clone())
        .partition(|v| vf.unchecked.contains(v));
    let cond = sql::values_condition(s.cfg.kind, &vf.expr, &checked, &unchecked, vf.complete);
    if let Some(i) = ts.value_conds.iter().position(|v| v.column == vf.column) {
        let old = ts.value_conds.remove(i);
        if let Some(rest) = sql::remove_condition(&ts.filter, &old.cond) {
            ts.filter = rest;
        }
    }
    if let Some(cond) = cond {
        ts.filter = sql::and(&ts.filter, &cond);
        ts.value_conds.push(ValueCond {
            column: vf.column,
            cond,
            unchecked: unchecked.into_iter().collect(),
        });
    }
    ts.page = 0;
    reload(tab, s);
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
    tab.begin_job();
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

/// Whether `sql` is a single UPDATE or DELETE (what "Prévia" works on).
pub fn previewable(sql: &str) -> bool {
    sql::statement_ranges(sql, false).len() == 1
        && sql::statement_words_first(sql).is_some_and(|w| w == "UPDATE" || w == "DELETE")
}

/// Starts a preview of an UPDATE/DELETE.
pub fn preview(tab: &mut Tab, s: &Session, sql: String) {
    let Some(ed) = tab.editor_mut() else { return };
    ed.preview = Some(PreviewState {
        sql: sql.clone(),
        data: None,
    });
    tab.error = None;
    tab.begin_job();
    s.send(Job::Preview { tab: tab.id, sql });
}

/// "Repetir a cada": runs the tab's last query again, quietly (no history entry, the
/// editable source and the selection stay).
pub fn rerun(tab: &mut Tab, s: &Session) {
    if has_unsaved(tab) {
        return;
    }
    let Some(ed) = tab.editor_mut() else { return };
    if ed.last_query.trim().is_empty() {
        return;
    }
    ed.last_run = Some(Instant::now());
    let sql = ed.last_query.clone();
    let id = tab.id;
    tab.send_rows_job(s, Job::Query { tab: id, sql });
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
    tab.begin_job();
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
    let cols = editable_ref(&tab.kind).map_or(&no_cols, |ts| &ts.cols);
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
                // An editable editor result knows its table too.
                TabKind::Sql(ed) if let Some(src) = &ed.source => k.qualified(&src.schema, &src.table),
                _ => "tabela".to_string(),
            };
            return Some(Effect::Copy(match how {
                CopyAs::Json => values::row_json(res, cols, *row),
                CopyAs::Insert => sql::insert_row(k, &table, &res.columns, &res.rows[*row]),
            }));
        }
        GridEvent::OpenValue(row, col) => {
            let (row, col) = (*row, *col);
            let column = res.columns[col].clone();
            let info = cols.iter().find(|c| c.name == column);
            let original = res.rows[row][col].clone();
            let text = original.clone().unwrap_or_default();
            let data_type = info.map_or(String::new(), |c| c.data_type.clone());
            let json_column = data_type.starts_with("json");
            let is_json = values::looks_like_json(&text);
            tab.selected = Some((row, col));
            tab.value_editor = Some(ValueEditor {
                row,
                col,
                // An expression of an editor's SELECT isn't a column that can be written.
                editable: editable && info.is_some(),
                json_column,
                compact: is_json && !text.contains('\n'),
                text: if is_json { values::json_pretty(&text) } else { text },
                null: original.is_none(),
                original,
                column,
                data_type,
            });
            return None;
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
        GridEvent::FilterValues(col) => {
            let name = res.columns[*col].clone();
            open_value_filter(tab, s, &name);
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
                    tab.begin_job();
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
        GridEvent::CopyMarkdown => {
            let view = tab.view();
            let range = tab.range().unwrap_or(CellRange {
                r0: 0,
                r1: res.rows.len().saturating_sub(1),
                cols: view,
            });
            if res.rows.is_empty() {
                return None;
            }
            let overlay = editable_ref(&tab.kind).and_then(|ts| ts.overlay(res));
            let shown = overlay.as_ref().map_or(res, |o| &o.result);
            return Some(Effect::Copy(values::to_markdown(shown, &range, &kinds)));
        }
        GridEvent::CopyCells { headers } => {
            let range = tab.selection()?;
            // Copy what the grid shows, pending edits included.
            let overlay = editable_ref(&tab.kind).and_then(|ts| ts.overlay(res));
            let shown = overlay.as_ref().map_or(res, |o| &o.result);
            return Some(Effect::Copy(values::to_tsv(shown, &range, *headers)));
        }
        GridEvent::FollowIncoming(row, i) => {
            let TabKind::Table(ts) = &tab.kind else {
                return None;
            };
            let fk = ts.incoming.get(*i)?;
            let col = res.columns.iter().position(|c| *c == fk.target)?;
            let value = res.rows[*row][col].as_deref()?;
            let filter = sql::filter_condition(k, &fk.column, FilterOp::Eq, Some(value));
            return Some(Effect::OpenFiltered {
                schema: fk.schema.clone(),
                table: fk.table.clone(),
                filter,
            });
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
    let is_table = matches!(tab.kind, TabKind::Table(_));
    let ts = editable_mut(&mut tab.kind)?;
    if !editable {
        return None;
    }
    match event {
        // Editor results keep the order their query asked for.
        GridEvent::Sort(_) if !is_table => {}
        // Expressions and aliases in an editor's SELECT aren't columns of the table.
        GridEvent::StartEdit(_, col) if !ts.cols.iter().any(|c| c.name == res.columns[col]) => {
            tab.error = Some(format!(
                "\"{}\" não é uma coluna de {}: não dá para editar",
                res.columns[col], ts.table
            ));
        }
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
        GridEvent::AskFill => tab.fill_prompt = Some(String::new()),
        GridEvent::FillSelection(value) => {
            let Some(sel) = &selection else { return None };
            tab.fill_prompt = None;
            for row in sel.r0..=sel.r1 {
                for &col in &sel.cols {
                    if let Err(e) = set_cell(ts, res, &kinds, k, (row, col), value.clone()) {
                        tab.error = Some(e);
                        return None;
                    }
                }
            }
        }
        GridEvent::FillDown => {
            let Some(sel) = &selection else { return None };
            // The first row as the grid shows it, pending edits included.
            let overlay = ts.overlay(res);
            let shown = overlay.as_ref().map_or(res, |o| &o.result);
            let top: Vec<Option<String>> = sel.cols.iter().map(|&c| shown.rows[sel.r0][c].clone()).collect();
            for row in sel.r0 + 1..=sel.r1 {
                for (i, &col) in sel.cols.iter().enumerate() {
                    if let Err(e) = set_cell(ts, res, &kinds, k, (row, col), top[i].clone()) {
                        tab.error = Some(e);
                        return None;
                    }
                }
            }
        }
        GridEvent::DeleteRows => {
            let Some(sel) = &selection else { return None };
            for row in sel.r0..=sel.r1 {
                let key = ts.row_key(res, row);
                if !ts
                    .changes
                    .iter()
                    .any(|c| matches!(c, Change::Delete { key: k2 } if *k2 == key))
                {
                    ts.changes.push(Change::Delete { key });
                }
            }
        }
        GridEvent::Select(..)
        | GridEvent::CopyMarkdown
        | GridEvent::FollowIncoming(..)
        | GridEvent::ColumnStats(..)
        | GridEvent::FilterValues(..)
        | GridEvent::OpenValue(..)
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
    if !ts.cols.iter().any(|c| c.name == res.columns[col]) {
        return Err(format!(
            "\"{}\" não é uma coluna de {}: não dá para editar",
            res.columns[col], ts.table
        ));
    }
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

/// The table state behind a tab's grid edits: a table tab's own, or the source table of an
/// editable editor result (once its columns are known).
/// The table whose rows the grid edits: a table tab's, or the one an editor's SELECT read from
/// (once its columns arrived).
pub fn editable_mut(kind: &mut TabKind) -> Option<&mut TableState> {
    match kind {
        TabKind::Table(ts) => Some(ts),
        TabKind::Sql(ed) => ed.source.as_deref_mut().filter(|s| !s.cols.is_empty()),
        _ => None,
    }
}

pub fn editable_ref(kind: &TabKind) -> Option<&TableState> {
    match kind {
        TabKind::Table(ts) => Some(ts),
        TabKind::Sql(ed) => ed.source.as_deref().filter(|s| !s.cols.is_empty()),
        _ => None,
    }
}

/// Undoes the most recent pending change.
pub fn undo_change(tab: &mut Tab) {
    if let Some(ts) = editable_mut(&mut tab.kind) {
        ts.changes.pop();
    }
}

pub fn discard_changes(tab: &mut Tab) {
    if let Some(ts) = editable_mut(&mut tab.kind) {
        ts.changes.clear();
        ts.confirm_save = false;
    }
}

/// Writes every pending change in one transaction, then reloads the page (or re-runs the
/// editor's SELECT).
pub fn save_changes(tab: &mut Tab, s: &Session) {
    let is_table = matches!(tab.kind, TabKind::Table(_));
    let query = tab.editor().map(|ed| ed.source_query.clone()).unwrap_or_default();
    let Some(ts) = editable_mut(&mut tab.kind) else {
        return;
    };
    ts.confirm_save = false;
    if ts.changes.is_empty() {
        return;
    }
    let stmts = ts.change_statements(s.cfg.kind);
    let refresh = if is_table { ts.page_sql(s.cfg.kind) } else { query };
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
    editable_ref(&tab.kind).is_some_and(|ts| !ts.changes.is_empty())
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
            schema_cols: vec![],
            fks: vec![],
            objects: vec![],
            databases: vec![],
            current_db: String::new(),
            current_schema: String::new(),
            txn: TxState::default(),
        };
        (s, jobs, replies)
    }

    pub(super) fn table_tab() -> Tab {
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
    fn editing_the_open_connection_applies_what_it_can() {
        let (mut s, _jobs, _replies) = fake_session(Kind::Postgres);
        let mut edited = s.cfg.clone();
        edited.preview_writes = true;
        edited.name = "produção".into();
        assert!(apply_live_settings(&mut s, &edited), "nothing needs a reconnect");
        assert!(s.cfg.preview_writes && s.cfg.name == "produção");
        edited.host = "outro-servidor".into();
        assert!(
            !apply_live_settings(&mut s, &edited),
            "a new host waits for a reconnect"
        );
        assert_eq!(s.cfg.host, "localhost", "and isn't applied live");
    }

    #[test]
    fn bulk_edits_on_a_selection() {
        let (s, _jobs, _replies) = fake_session(Kind::Postgres);
        let mut tab = table_tab();
        if let Some(r) = &mut tab.result {
            r.rows
                .push(vec![Some("8".into()), Some("t".into()), Some("bia".into())]);
            r.rows
                .push(vec![Some("9".into()), Some("f".into()), Some("caio".into())]);
        }
        let sets = |tab: &Tab| -> Vec<(Vec<Option<String>>, String, Option<String>)> {
            let TabKind::Table(ts) = &tab.kind else { panic!() };
            ts.changes
                .iter()
                .filter_map(|c| match c {
                    Change::Set { key, col, value, .. } => Some((key.clone(), col.clone(), value.clone())),
                    Change::Delete { .. } => None,
                })
                .collect()
        };
        let key = |id: &str| vec![Some(id.to_string())];
        // "nome" of the first two rows set to one value.
        handle_grid_event(&mut tab, &s, GridEvent::Select(0, 2), true);
        handle_grid_event(&mut tab, &s, GridEvent::Extend(1, 2), true);
        handle_grid_event(&mut tab, &s, GridEvent::FillSelection(Some("igual".into())), true);
        assert_eq!(
            sets(&tab),
            [
                (key("7"), "nome".into(), Some("igual".into())),
                (key("8"), "nome".into(), Some("igual".into()))
            ]
        );
        // Fill down takes the first row as shown (with its pending edit).
        handle_grid_event(&mut tab, &s, GridEvent::Select(0, 2), true);
        handle_grid_event(&mut tab, &s, GridEvent::Extend(2, 2), true);
        handle_grid_event(&mut tab, &s, GridEvent::FillDown, true);
        assert!(sets(&tab).contains(&(key("9"), "nome".into(), Some("igual".into()))));
        // Delete marks every selected row once.
        handle_grid_event(&mut tab, &s, GridEvent::DeleteRows, true);
        handle_grid_event(&mut tab, &s, GridEvent::DeleteRows, true);
        let TabKind::Table(ts) = &tab.kind else { panic!() };
        assert_eq!(
            ts.changes
                .iter()
                .filter(|c| matches!(c, Change::Delete { .. }))
                .count(),
            3
        );
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
    fn sql_files_open_save_and_guard_unsaved_changes() {
        let (s, _jobs, _replies) = fake_session(Kind::Postgres);
        let mut app = app_with(s);
        let dir = std::env::temp_dir().join(format!("dblite-tabfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("consulta.sql");
        std::fs::write(&path, "SELECT 1;").unwrap();
        app.open_file(path.clone());
        assert_eq!(app.tabs[0].title, "consulta.sql");
        assert!(!file_dirty(&app.tabs[0]));
        if let Some(text) = app.tabs[0].editor_mut().map(|ed| &mut ed.text) {
            text.push_str("\nSELECT 2;");
        }
        assert!(file_dirty(&app.tabs[0]));
        app.close_tab(0);
        assert_eq!(app.tabs.len(), 1, "the first close only warns");
        app.save_file(0, false);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "SELECT 1;\nSELECT 2;");
        assert!(!file_dirty(&app.tabs[0]));
        // Opening it again focuses the tab instead of a second copy.
        app.open_sql(String::new());
        app.open_file(path.clone());
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active, 0);
        // The session remembers the file.
        let snap = app.snapshot();
        assert!(matches!(&snap.tabs[0], SavedTab::Sql { file: Some(f), .. } if *f == path));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn closed_tabs_come_back_with_their_name() {
        let (s, _jobs, _replies) = fake_session(Kind::Postgres);
        let mut app = app_with(s);
        app.open_sql("select 1".into());
        app.tabs[0].title = "Relatório".into();
        app.tabs[0].renamed = true;
        app.open_table("public", "clientes");
        if let TabKind::Table(ts) = &mut app.tabs[1].kind {
            ts.filter = "ativo".into();
        }
        app.close_tab(0);
        app.close_tab(0);
        assert!(app.tabs.is_empty());
        app.reopen_closed_tab();
        assert!(
            matches!(&app.tabs[0].kind, TabKind::Table(ts) if ts.filter == "ativo"),
            "last closed first"
        );
        app.reopen_closed_tab();
        assert_eq!(app.tabs[1].title, "Relatório");
        app.duplicate_tab(1);
        assert_eq!(app.tabs[2].title, "Relatório (cópia)");
        app.close_others(2, false);
        assert_eq!(app.tabs.len(), 1);
    }

    /// A compact JSON opens laid out, is edited there and goes back compact as a pending
    /// change; a read-only result opens it only to look.
    #[test]
    fn big_value_editor_round_trip() {
        let (s, _jobs, _replies) = fake_session(Kind::Postgres);
        let mut tab = super::tests::table_tab();
        if let TabKind::Table(ts) = &mut tab.kind {
            ts.cols.push(ColumnInfo {
                name: "dados".into(),
                data_type: "jsonb".into(),
                pk: false,
                nullable: true,
                has_default: false,
                references: None,
            });
        }
        let res = tab.result.as_mut().unwrap();
        res.rows
            .push(vec![Some("8".into()), Some("t".into()), Some("bia".into())]);
        res.columns.push("dados".into());
        for (i, row) in res.rows.iter_mut().enumerate() {
            row.push((i == 0).then(|| r#"{"preco":1520.30,"tags":["a"]}"#.to_string()));
        }
        let col = res.columns.len() - 1;
        handle_grid_event(&mut tab, &s, GridEvent::OpenValue(0, col), true);
        let ve = tab.value_editor.as_mut().unwrap();
        assert!(ve.editable && ve.json_column && ve.compact);
        assert_eq!(
            ve.text,
            "{\n  \"preco\": 1520.30,\n  \"tags\": [\n    \"a\"\n  ]\n}"
        );
        assert_eq!(ve.value(), ve.original, "opening alone changes nothing");
        ve.text = ve.text.replace("1520.30", "99.90");
        let v = ve.value();
        assert_eq!(v.as_deref(), Some(r#"{"preco":99.90,"tags":["a"]}"#));
        tab.value_editor = None;
        handle_grid_event(&mut tab, &s, GridEvent::Commit(0, col, v), true);
        assert!(has_unsaved(&tab));
        // Not editable: opens anyway, to read.
        handle_grid_event(&mut tab, &s, GridEvent::OpenValue(1, col), false);
        let ve = tab.value_editor.as_ref().unwrap();
        assert!(!ve.editable && ve.null);
    }

    /// The cached overlay follows every change to the edits and to the result.
    #[test]
    fn overlay_cache_never_shows_stale_edits() {
        let mut ts = TableState::new("public", "t", None);
        ts.cols = vec![ColumnInfo {
            name: "id".into(),
            data_type: "integer".into(),
            pk: true,
            nullable: false,
            has_default: false,
            references: None,
        }];
        let res = QueryResult {
            columns: vec!["id".into(), "nome".into()],
            rows: vec![
                vec![Some("1".into()), Some("a".into())],
                vec![Some("2".into()), Some("b".into())],
            ],
            ..Default::default()
        };
        let set = |row: &str, v: &str| Change::Set {
            key: vec![Some(row.into())],
            col: "nome".into(),
            value: Some(v.into()),
            before: None,
        };
        assert!(ts.overlay(&res).is_none());
        ts.changes.push(set("1", "x"));
        assert_eq!(ts.overlay(&res).unwrap().result.rows[0][1].as_deref(), Some("x"));
        // Same number of changes, different value.
        ts.changes[0] = set("1", "y");
        assert_eq!(ts.overlay(&res).unwrap().result.rows[0][1].as_deref(), Some("y"));
        ts.changes.push(Change::Delete {
            key: vec![Some("2".into())],
        });
        assert!(ts.overlay(&res).unwrap().deleted.contains(&1));
        // A new result (same shape) is overlaid again.
        let mut other = res.clone();
        other.rows[0][1] = Some("z".into());
        other.elapsed = std::time::Duration::from_millis(9);
        let o = ts.overlay(&other).unwrap();
        assert_eq!(o.result.rows[0][1].as_deref(), Some("y"));
        assert!(o.changed.contains(&(0, 1)));
    }

    #[test]
    fn errors_are_found_in_the_editor_text() {
        let e = crate::error::DbError::MySql(mysql::Error::MySqlError(mysql::MySqlError {
            state: "42S22".into(),
            message: "Unknown column 'emial' in 'field list'".into(),
            code: 1054,
        }));
        // The statement ran with Ctrl+Enter is the second one; accents before it count as chars.
        let text = "-- relatório\nSELECT 1;\nSELECT nome, emial FROM clientes;";
        let spot = locate_error(text, "SELECT nome, emial FROM clientes", &e).unwrap();
        assert_eq!((spot.line, spot.column), (3, 14));
        let chars: String = text
            .chars()
            .skip(spot.start)
            .take(spot.end - spot.start)
            .collect();
        assert_eq!(chars, "emial");
        // With :parameters bound, the SQL sent isn't in the editor: no place to mark.
        assert_eq!(
            locate_error(text, "SELECT nome, emial FROM clientes WHERE id = 1", &e),
            None
        );
    }

    #[test]
    fn a_tab_is_timed_from_its_first_job_to_its_last_reply() {
        let mut tab = Tab::new(1, "t".into(), TabKind::Sql(Box::new(Editor::new(String::new()))));
        tab.begin_job();
        tab.begin_job();
        assert_eq!(tab.end_job(), None, "still waiting for one");
        assert!(tab.end_job().is_some(), "the last reply reports how long it took");
        assert_eq!(tab.end_job(), None, "a stray reply measures nothing");
        assert!(!tab.busy());
    }

    #[test]
    fn editor_results_of_one_table_become_editable() {
        let (mut s, jobs, replies) = fake_session(Kind::Postgres);
        s.tables = vec![TableRef {
            schema: "public".into(),
            name: "clientes".into(),
            is_view: false,
            estimate: None,
            search_key: "clientes".into(),
            size: None,
        }];
        let mut app = app_with(s);
        app.open_sql(String::new());
        let sql = "SELECT id, nome, upper(nome) AS grito FROM Clientes WHERE ativo".to_string();
        run_sql(&mut app.tabs[0], app.session.as_ref().unwrap(), sql.clone());
        let tab = app.tabs[0].id;
        let rows = QueryResult {
            columns: vec!["id".into(), "nome".into(), "grito".into()],
            rows: vec![vec![Some("7".into()), Some("ana".into()), Some("ANA".into())]],
            ..Default::default()
        };
        replies
            .send(Reply::Rows {
                tab,
                res: Box::new(Ok(rows)),
                notice: None,
            })
            .unwrap();
        app.poll();
        // The unquoted name resolves to the sidebar's table and its columns are asked for.
        let asked = jobs.try_iter().any(|j| matches!(j, Job::Columns { ref schema, ref table, .. } if schema == "public" && table == "clientes"));
        assert!(asked);
        let col = |name: &str, pk| ColumnInfo {
            name: name.into(),
            data_type: "text".into(),
            pk,
            nullable: true,
            has_default: false,
            references: None,
        };
        replies
            .send(Reply::Columns {
                tab,
                res: Ok(vec![col("id", true), col("nome", false)]),
                incoming: vec![],
            })
            .unwrap();
        app.poll();
        let s = app.session.as_ref().unwrap();
        let t = &mut app.tabs[0];
        handle_grid_event(t, s, GridEvent::Commit(0, 1, Some("bia".into())), true);
        // An expression isn't a column of the table.
        handle_grid_event(t, s, GridEvent::Commit(0, 2, Some("X".into())), true);
        assert!(t.error.as_deref().is_some_and(|e| e.contains("grito")));
        assert!(has_unsaved(t));
        // Running something else first would lose the edit: refused.
        run_sql(t, s, "SELECT 1".into());
        assert!(t.error.as_deref().is_some_and(|e| e.contains("pendentes")));
        save_changes(t, s);
        let Some(Job::Batch { stmts, refresh, .. }) = jobs.try_iter().last() else {
            panic!("expected a batch")
        };
        assert_eq!(
            stmts[0].render(Kind::Postgres),
            "UPDATE \"public\".\"clientes\" SET \"nome\" = 'bia' WHERE \"id\" = '7'"
        );
        assert_eq!(refresh, sql, "then the same SELECT runs again");
    }

    /// Check values, apply, reopen with the same checks (and without its own condition in the
    /// list query), then remove.
    #[test]
    fn value_filter_round_trip() {
        let (s, jobs, replies) = fake_session(Kind::Postgres);
        let mut app = app_with(s);
        app.open_table_filtered("public", "clientes", "ativo".into());
        let tab = app.tabs[0].id;
        let distinct = |jobs: &Receiver<Job>| {
            jobs.try_iter()
                .find_map(|j| match j {
                    Job::Distinct { sql, .. } => Some(sql),
                    _ => None,
                })
                .expect("a Distinct job")
        };
        let reply = |replies: &Sender<Reply>| {
            let row = |v: Option<&str>, n: &str| vec![v.map(String::from), Some(n.to_string())];
            replies
                .send(Reply::Distinct {
                    tab,
                    res: Ok(QueryResult {
                        rows: vec![row(Some("SP"), "50"), row(Some("RJ"), "30"), row(None, "2")],
                        ..Default::default()
                    }),
                })
                .unwrap();
        };
        let s = app.session.as_ref().unwrap();
        open_value_filter(&mut app.tabs[0], s, "uf");
        assert!(distinct(&jobs).contains("WHERE ativo GROUP BY 1"));
        reply(&replies);
        app.poll();
        fn t(app: &mut App) -> &mut TableState {
            match &mut app.tabs[0].kind {
                TabKind::Table(ts) => ts,
                _ => unreachable!(),
            }
        }
        let vf = t(&mut app).value_filter.as_mut().unwrap();
        assert!(vf.complete && vf.values.as_ref().unwrap().as_ref().unwrap().len() == 3);
        vf.unchecked.insert(Some("RJ".into()));
        let s = app.session.as_ref().unwrap();
        apply_value_filter(&mut app.tabs[0], s);
        assert_eq!(
            t(&mut app).filter,
            "(ativo) AND (\"uf\" NOT IN ('RJ') OR \"uf\" IS NULL)"
        );
        assert!(t(&mut app).value_filter.is_none());

        let s = app.session.as_ref().unwrap();
        open_value_filter(&mut app.tabs[0], s, "uf");
        assert!(
            distinct(&jobs).contains("WHERE ativo GROUP BY 1"),
            "its own condition is left out"
        );
        let vf = t(&mut app).value_filter.as_ref().unwrap();
        assert!(vf.unchecked.contains(&Some("RJ".into())));
        reply(&replies);
        app.poll();
        t(&mut app).value_filter.as_mut().unwrap().unchecked.clear();
        let s = app.session.as_ref().unwrap();
        apply_value_filter(&mut app.tabs[0], s);
        assert_eq!(t(&mut app).filter, "ativo");
        assert!(t(&mut app).value_conds.is_empty());
    }

    /// Progress keeps the tab busy, the end frees it; a closed tab's reply is dropped; the
    /// connection's own replies reach the session.
    #[test]
    fn replies_reach_their_tab_or_the_session() {
        let (s, _jobs, replies) = fake_session(Kind::Postgres);
        let mut app = app_with(s);
        let file = std::env::temp_dir().join("dblite-poll-script.sql");
        std::fs::write(&file, "SELECT 1;").unwrap();
        app.open_script(file);
        let tab = app.tabs[0].id;
        app.tabs[0].begin_job();
        let progress = worker::ScriptProgress {
            statements: 3,
            ..Default::default()
        };
        replies.send(Reply::ScriptProgress { tab, progress }).unwrap();
        replies.send(Reply::Count { tab: 999, res: Ok(1) }).unwrap();
        app.poll();
        assert!(app.tabs[0].busy());
        assert!(matches!(&app.tabs[0].kind, TabKind::Script(st) if st.progress.statements == 3));
        replies
            .send(Reply::ScriptDone {
                tab,
                progress,
                error: None,
            })
            .unwrap();
        replies
            .send(Reply::Location(Ok((
                vec!["postgres".into()],
                "postgres".into(),
                "public".into(),
            ))))
            .unwrap();
        app.poll();
        assert!(!app.tabs[0].busy());
        assert!(matches!(&app.tabs[0].kind, TabKind::Script(st) if st.outcome == Some(None)));
        assert_eq!(app.session.as_ref().unwrap().current_schema, "public");
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
