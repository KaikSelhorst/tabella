//! What a tab holds: its kind, the table and editor state, and the panes.

use super::*;

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
    /// Foreign keys in other tables that point here ("rows pointing to this one").
    pub incoming: Vec<IncomingFk>,
    pub page: usize,
    pub filter: String,
    /// Text searched in every column (the search box), on top of `filter`.
    pub search: String,
    /// Columns sorted by, the first being the main one (Shift+click on a header adds more).
    pub order: Vec<(String, bool)>,
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
    pub option_pick: Option<(usize, usize)>,
    /// The "new row" form, while open.
    pub insert: Option<Vec<InsertField>>,
    /// The CSV import dialog, while open.
    pub import: Option<ImportForm>,
    /// "Filter values" of a column, while open.
    pub value_filter: Option<ValueFilter>,
    /// The last `overlay`, and what it was built from: the grid asks for it every frame.
    overlay_cache: std::cell::RefCell<Option<(u64, std::rc::Rc<Overlay>)>>,
    /// Conditions "Filter values" put in `filter`, so applying it again replaces them.
    pub value_conds: Vec<ValueCond>,
}

/// A column's values and how many rows have each.
pub type ValueCounts = Vec<(Option<String>, i64)>;

/// "Filter values": a column's values to check and uncheck, like a spreadsheet filter.
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

/// "Import CSV" dialog: a file, how to read it, and where each of its columns goes.
#[derive(Default)]
pub struct ImportForm {
    pub path: String,
    pub has_header: bool,
    pub empty_is_null: bool,
    /// Preview of `path` as last read (path and header flag it was read with).
    pub preview: Option<(String, bool, Result<import::Preview, import::ImportError>)>,
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
pub(super) const EXACT_COUNT_LIMIT: i64 = 1_000_000;

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
            order: Vec::new(),
            edit: None,
            total: None,
            estimate,
            counted_filter: None,
            changes: Vec::new(),
            confirm_save: false,
            option_pick: None,
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
                    .and_then(|i| res.rows.cell(row, i).map(str::to_owned))
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
        (res.rows.addr(), res.rows.len(), res.columns.len(), res.elapsed).hash(&mut h);
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
            // Rows come below, with the edits in (copied once, not cell by cell).
            result: QueryResult {
                columns: res.columns.clone(),
                rows: Default::default(),
                affected: res.affected,
                truncated: res.truncated,
                elapsed: res.elapsed,
                previous: Vec::new(),
                timings: Vec::new(),
            },
            changed: HashSet::new(),
            deleted: HashSet::new(),
        };
        let mut edits: std::collections::HashMap<(usize, usize), Option<&str>> = Default::default();
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
            key.extend(
                key_at
                    .iter()
                    .map(|i| i.and_then(|i| res.rows.cell(row, i).map(str::to_owned))),
            );
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
                            edits.insert((row, ci), value.as_deref());
                            out.changed.insert((row, ci));
                        }
                    }
                }
            }
        }
        out.result.rows = res.rows.map(|r, c, v| edits.get(&(r, c)).copied().unwrap_or(v));
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
                order: &self.order,
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
    /// "New table" / "Edit structure".
    Design(Box<Design>),
    /// "Search value in all tables".
    Search(Box<SearchState>),
    /// ER diagram of a schema.
    Diagram(Box<DiagramState>),
    /// Every query run, searchable.
    History(Box<HistoryState>),
    /// A `.sql` file run without opening it in an editor.
    Script(Box<ScriptState>),
    /// "Export .sql": tables (and a schema's objects) written as a script.
    Dump(Box<DumpState>),
    /// "Compare structure" between two schemas or databases.
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
    /// What was compared, as shown with the result ("dev · public → staging · public").
    pub compared: String,
    /// The SQL runs here (same connection and database), or must be run on the target.
    pub same_connection: bool,
    /// "Save .sql": the file written.
    pub saved: Option<PathBuf>,
}

/// What "Export .sql" covers.
pub enum DumpScope {
    Table(String, String),
    Schema(String),
    /// Every schema of the database in use (MySQL: that database).
    Database,
}

pub struct DumpState {
    /// For the title: "table public.customers", "schema public", "database shop".
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
    /// A plan kept to compare the next ones with ("Pin to compare").
    pub pinned_plan: Option<Plan>,
    /// Values asked for `:parameters` before running.
    pub params: Option<ParamPrompt>,
    /// Last value typed for each parameter name.
    pub param_values: std::collections::HashMap<String, String>,
    /// "Preview" of an UPDATE/DELETE: rows it would touch.
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
    /// "Repeat every": seconds between automatic re-runs of `last_query`.
    pub repeat: Option<u64>,
    pub last_query: String,
    /// What `last_query` is, worked out once when it is set (not on every frame): a read, and a
    /// single statement.
    pub last_reads: bool,
    pub last_single: bool,
    pub last_run: Option<Instant>,
    /// Where the last error is in `text` (chars), underlined until the text changes.
    pub error_at: Option<(usize, usize)>,
    /// A result kept to compare the next ones against, and the comparison's settings.
    pub pinned: Option<Box<Pinned>>,
    pub comparing: bool,
    /// Column that pairs the rows up (`None`: by position).
    pub compare_key: Option<String>,
    pub hide_equal: bool,
    /// Move the cursor to `error_at` on the next frame.
    pub error_jump: bool,
}

pub struct Pinned {
    pub result: QueryResult,
    pub sql: String,
    /// When it was pinned (`history::now`).
    pub at: u64,
}

impl Editor {
    /// Remembers the query that just ran, and what kind it is.
    pub fn set_last_query(&mut self, sql: String) {
        self.last_reads = !sql.trim().is_empty() && !sql::script_risk(&sql).writes;
        self.last_single = sql::statement_ranges(&sql, false).len() == 1;
        self.last_query = sql;
    }

    /// Keeps `res` (what `sql` returned) to compare later results against.
    pub fn pin(&mut self, res: &QueryResult, key_hint: Option<String>) {
        self.pinned = Some(Box::new(Pinned {
            result: res.clone(),
            sql: self.last_query.clone(),
            at: history::now(),
        }));
        self.comparing = false;
        // A primary key, an "id", or the first column: what usually identifies a row.
        self.compare_key = key_hint
            .filter(|k| res.columns.contains(k))
            .or_else(|| res.columns.iter().find(|c| c.eq_ignore_ascii_case("id")).cloned())
            .or_else(|| res.columns.first().cloned());
    }

    pub fn new(text: String) -> Self {
        Editor {
            text,
            plan: None,
            pinned_plan: None,
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
            last_reads: false,
            last_single: false,
            last_run: None,
            error_at: None,
            error_jump: false,
            pinned: None,
            comparing: false,
            compare_key: None,
            hide_equal: true,
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
    /// The slowest statements of the last run, slowest first.
    pub slowest: Vec<crate::db::StatementTime>,
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

/// "Activity" tab: sessions on the server, optionally refreshed every few seconds.
pub struct ActivityState {
    pub view: ActivityView,
    /// The "Server" view's data (`None` until it first arrives).
    pub server: Option<Result<ServerInfo, String>>,
    pub auto_refresh: bool,
    pub last_refresh: Option<Instant>,
    /// Session waiting for the user to confirm `(id, terminate)`.
    pub confirm_kill: Option<(i64, bool)>,
    /// Why the last cancel/terminate had no effect (kept until the next one).
    pub kill_error: Option<String>,
    /// The answer to "Suggest index", shown in a dialog until closed.
    pub advice: Option<Result<crate::advisor::Advice, String>>,
}

/// "Structure" tab: columns, indexes, foreign keys and DDL of one table.
pub struct StructState {
    pub schema: String,
    pub table: String,
    pub data: Option<TableStructure>,
    /// A column to point at (Ctrl+click on it in the editor): its row is highlighted.
    pub focus: Option<String>,
    /// Bumped each time `focus` is asked for, so the view scrolls to it once per request.
    pub focus_seq: u64,
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
    /// The row a foreign-key column points at (its value can be picked from there).
    pub references: Option<crate::db::FkRef>,
    pub data_type: String,
    pub kind: ValueKind,
    /// The allowed values when the column is an ENUM.
    pub enum_values: Vec<String>,
    pub nullable: bool,
    pub mode: FieldMode,
    pub text: String,
}

/// Two panes side by side, each with its own tab strip and its own selected tab. A tab belongs
/// to the pane `Tab::right` says; the focused pane is the one holding the active tab.
pub struct Split {
    /// The tab each pane shows: `[left, right]`.
    pub shown: [u64; 2],
}

pub struct Tab {
    pub id: u64,
    /// In the right pane of the split view.
    pub right: bool,
    /// Kept at the start of its strip, without a close button, and out of "Close others".
    pub pinned: bool,
    pub title: String,
    pub kind: TabKind,
    pub result: Option<QueryResult>,
    pub error: Option<String>,
    /// Jobs sent and not answered yet. The tab is busy while this is above zero.
    pub(super) pending: u32,
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
    /// Choosing a foreign key's value from the referenced table.
    pub fk_picker: Option<FkPicker>,
    /// Last export of this tab: file and row count, shown until dismissed.
    pub exported: Option<(PathBuf, u64)>,
    /// Last "copy to clipboard" of the result: the format and the row count, shown until dismissed.
    pub copied: Option<(&'static str, u64)>,
    /// A script's result sets when it returned more than one, in order. The one on screen lives
    /// in `result`; its slot here is an empty placeholder.
    pub sets: Vec<QueryResult>,
    /// Time of each statement of the script that produced the results (all sets share it).
    pub timings: Vec<crate::db::StatementTime>,
    pub set_shown: usize,
    /// "Set value in selection": the value being typed.
    pub fill_prompt: Option<String>,
    /// "Set with an expression": the form for a bulk UPDATE of a table's column.
    pub bulk_prompt: Option<BulkPrompt>,
    pub col_stats: Option<StatsView>,
    /// Columns hidden from the grid, and columns pinned to its left edge (by name, so they
    /// survive reloads and paging).
    pub hidden_cols: Vec<String>,
    pub pinned_cols: Vec<String>,
    /// Widths the user gave columns, by name (remembered across tabs and runs).
    pub col_widths: std::collections::HashMap<String, f32>,
    /// The user gave the tab its own name.
    pub renamed: bool,
}

/// "Column statistics" dialog.
pub struct StatsView {
    pub column: String,
    /// What the numbers cover: the whole (filtered) table or just the loaded rows.
    pub scope: String,
    /// `None` while the database is still counting.
    pub data: Option<Result<values::ColumnStats, String>>,
}

/// "Preview" of an UPDATE/DELETE: what it would touch, before deciding to run it.
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
            right: false,
            pinned: false,
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
            fk_picker: None,
            exported: None,
            copied: None,
            sets: Vec::new(),
            timings: Vec::new(),
            set_shown: 0,
            fill_prompt: None,
            bulk_prompt: None,
            col_stats: None,
            hidden_cols: Vec::new(),
            pinned_cols: Vec::new(),
            col_widths: Default::default(),
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
    pub(super) fn send_rows_job(&mut self, s: &Session, job: Job) {
        self.begin_job();
        s.send(job);
    }

    /// Counts a job sent for this tab; the first of a batch starts the clock.
    pub(super) fn begin_job(&mut self) {
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
