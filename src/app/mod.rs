//! Application state and the logic that acts on it. The `ui` modules draw this state and turn
//! clicks and keys into calls here; nothing in this file draws anything.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Instant;

mod connections;
mod dialogs;
mod files;
mod grid_events;
mod jobs;
mod open;
mod replies;
mod run;
mod tab;
mod table_edit;
mod window;

pub use connections::create_database_file;
pub use dialogs::{
    AfterMaster, BulkPrompt, ConnForm, ConnTest, Dialog, FavPrompt, FkPicker, MasterMode, MasterPrompt,
    Palette, PathPrompt, PathUse, PickFor, TableActionPrompt, ValueEditor, fk_search, open_fk_picker,
};
pub use files::file_dirty;
use files::{MAX_CLOSED, saved_tab};
pub use grid_events::handle_grid_event;
use jobs::resolve_table;
pub use jobs::{
    cancel_dump, cancel_script, cancel_search, compare_script, copy_result, kill_session,
    open_duplicate_form, open_insert_form, refresh_activity, reset_query_stats, start_compare, start_dump,
    start_export, start_script, start_search, submit_insert, suggest_index,
};
use run::{LONG_JOB, request_count};
pub use run::{
    apply_live_settings, can_load_more, exec, explain, load_more, locate_error, preview, previewable, reload,
    rerun, run_sql,
};
use tab::EXACT_COUNT_LIMIT;
pub use tab::{
    ActivityState, Change, CompareState, DumpScope, DumpState, Edit, Editor, FieldMode, HistoryState,
    ImportForm, InsertField, ParamPrompt, Pinned, PreviewState, RunMode, ScriptState, SearchState, Split,
    StatsView, StructState, Tab, TabKind, TableState, ValueCond, ValueFilter,
};
pub use table_edit::{
    apply_value_filter, discard_changes, editable_mut, editable_ref, filter_by_value, has_unsaved,
    load_values, open_import, open_value_filter, refresh_import_preview, save_changes, submit_import,
    undo_change,
};
use table_edit::{enum_value, set_cell};
pub use window::{Waker, Window};

use crate::config;
use crate::db::{
    ActivityView, ColumnInfo, ConnConfig, DbObject, FkEdge, IncomingFk, Kind, PreviewData, QueryResult,
    SchemaColumn, ServerInfo, TableRef, TableStructure,
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
use crate::secrets::Secret;
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
    /// What each of the session's connections is running, for cancelling a tab's statement.
    running: Vec<Arc<worker::Running>>,
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
    /// Cells saved from a grid inside the open transaction: not committed yet. Emptied when
    /// the transaction ends (commit, rollback, or the connection losing it).
    pub uncommitted: HashSet<UncommittedCell>,
}

/// A cell written inside the open transaction.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UncommittedCell {
    pub schema: String,
    pub table: String,
    /// The row's primary key values.
    pub key: Vec<Option<String>>,
    pub column: String,
}

impl Session {
    /// The cells of `res` (rows of `ts`'s table) written in the open transaction.
    pub fn uncommitted_in(&self, ts: &TableState, res: &QueryResult) -> HashSet<(usize, usize)> {
        let mut out = HashSet::new();
        let here: Vec<&UncommittedCell> = self
            .uncommitted
            .iter()
            .filter(|c| c.schema == ts.schema && c.table == ts.table)
            .collect();
        if here.is_empty() {
            return out;
        }
        for row in 0..res.rows.len() {
            let key = ts.row_key(res, row);
            for c in here.iter().filter(|c| c.key == key) {
                if let Some(col) = res.columns.iter().position(|n| *n == c.column) {
                    out.insert((row, col));
                }
            }
        }
        out
    }

    /// Whether the sidebar lists this as a view (no rows to insert, no table DDL).
    pub fn is_view(&self, schema: &str, table: &str) -> bool {
        self.tables
            .iter()
            .any(|t| t.is_view && t.schema == schema && t.name == table)
    }

    /// Lists the tables, columns, keys and objects again (sidebar, autocomplete).
    pub fn reload_tables(&self) {
        self.send(Job::Tables);
    }

    pub(in crate::app) fn send(&self, job: Job) {
        // Only fails when the worker is gone; `poll` reports that.
        let _ = self.tx.send(job);
    }

    /// Asks the server to stop the statement running for this tab (on whichever of the
    /// session's connections has it). Runs on its own thread: it may open a connection and
    /// must not block the UI.
    pub fn cancel(&self, tab: u64) {
        self.cancel_running(Some(tab));
    }

    /// Stops whatever the session's connections are running.
    pub fn cancel_all(&self) {
        self.cancel_running(None);
    }

    fn cancel_running(&self, tab: Option<u64>) {
        let running = self.running.clone();
        std::thread::spawn(move || {
            for r in running {
                r.cancel(tab);
            }
        });
    }
}

/// What the grid asks for. Produced by the UI, carried out by `handle_grid_event`.
pub enum GridEvent {
    Sort(String),
    /// Shift+click: this column joins the sort (or flips, or leaves it).
    SortAlso(String),
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
    /// "Filter values" of a column (table tabs).
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
    /// The distinct values of a column as `(a, b, c)`.
    CopyInList(usize),
    /// Ask for a value to put in every selected cell.
    AskFill,
    /// "Set with an expression…" for this column.
    AskBulk(usize),
    /// Every selected cell gets this value (`None`: NULL).
    FillSelection(Option<String>),
    /// Each selected column takes the value of the selection's first row (Ctrl+D).
    FillDown,
    /// Mark the selected rows for deletion.
    DeleteRows,
    /// Columns got new widths from dragging their edges (name, width).
    ColumnWidths(Vec<(String, f32)>),
}

/// A jump to a filtered table, with what "Back" needs to undo it.
struct NavStep {
    /// The tab the jump started from.
    from: u64,
    /// The table tab that got the filter, and the filter it had (None: the jump opened it).
    to: u64,
    previous_filter: Option<String>,
}

/// What `handle_grid_event` needs the app (or the clipboard) to do.
pub enum Effect {
    Copy(String),
    /// Remember these column widths for the table.
    ColumnWidths {
        schema: String,
        table: String,
        widths: Vec<(String, f32)>,
    },
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
    /// The window it runs in, for work started outside a frame (e.g. connecting after an
    /// unlock) and for its title and attention.
    host: Arc<dyn Window>,
    pub connections: Connections,
    pub session: Option<Session>,
    pub dialog: Option<Dialog>,
    pub tabs: Vec<Tab>,
    pub active: usize,
    /// Two tabs side by side. `active` is always the one with focus.
    pub split: Option<Split>,
    next_id: u64,
    pub filter: String,
    /// The schema (MySQL: database) just switched to: the sidebar opens it and folds the others
    /// once.
    pub schema_focus: Option<String>,
    pub notice: Option<Notice>,
    pub fps: Fps,
    pub library: Library,
    /// Foreign keys followed (and other jumps to a filtered table), for "Back".
    nav_back: Vec<NavStep>,
    /// Tabs from the last run, reopened once their connection is ready.
    restore: Option<session::Saved>,
    /// `.sql` files from the command line, opened once connected.
    launch_files: Vec<std::path::PathBuf>,
    pub window_state: WindowState,
    /// Row detail panel on the right of the grid.
    pub show_detail: bool,
    /// Sidebar lists tables by size (biggest first) instead of by name.
    pub sort_by_size: bool,
    /// Recently closed tabs, newest last.
    pub closed: Vec<SavedTab>,
    /// Ids of the tabs open last frame: a tab gone since gets its UI caches dropped.
    pub(crate) open_tab_ids: Vec<u64>,
    pub settings: crate::settings::Settings,
}

/// Saved connections and what it takes to edit and open them.
pub struct Connections {
    pub saved: Vec<ConnConfig>,
    pub secrets: SecretStore,
    pub master: Option<MasterPrompt>,
    pub form: Option<ConnForm>,
    /// "Test connection" in the form: running, or its outcome for the settings it was run with.
    pub conn_test: Option<ConnTest>,
    /// A backup or restore running on its own thread.
    backup: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
}

/// What the user keeps across runs besides connections: history, favorites, filters and the
/// per-table memory of widths, recent and starred tables.
pub struct Library {
    pub history: Vec<Entry>,
    history_writer: history::Writer,
    pub favorites: Vec<Favorite>,
    pub filters: Vec<SavedFilter>,
    /// Column widths dragged by the user, per connection and table.
    col_widths: crate::widths::Store,
    /// The tables opened last, per connection, for `Ctrl+P`.
    recent: crate::recent::Store,
    /// Tables starred in the sidebar, per connection.
    pub starred: crate::starred::Store,
}

/// The frames-per-second readout.
pub struct Fps {
    /// Redraw every frame at the monitor's rate (for the readout). Off by default: an idle
    /// app should use no CPU.
    pub max: bool,
    /// The readout in the corner (Ctrl+P), off unless asked for.
    pub show: bool,
    pub value: f32,
    frames: u32,
    since: Instant,
}

/// The window's size, title and closing.
#[derive(Default)]
pub struct WindowState {
    /// Current size, saved on exit.
    pub size: Option<[f32; 2]>,
    /// The title announces a finished job (cleared when the window is focused).
    title_changed: bool,
    /// Asked to close with a transaction open: ask what to do with it.
    pub quit_prompt: bool,
    /// Close once the transaction has been ended.
    pub quit_after_tx: bool,
}

impl App {
    /// For threads to wake the window up when their work is done.
    pub(in crate::app) fn waker(&self) -> Waker {
        let host = self.host.clone();
        Arc::new(move || host.wake())
    }

    #[cfg(test)]
    pub fn new(host: impl Window + 'static) -> Self {
        Self::with_launch(host, crate::cli::Launch::default())
    }

    /// Starts as the command line asked (see `cli`).
    pub fn with_launch(host: impl Window + 'static, launch: crate::cli::Launch) -> Self {
        let mut secrets = SecretStore::detect();
        let conns = config::load(&secrets);
        let history = history::load();
        let favorites = favorites::load();
        let filters = filters::load();
        let col_widths = crate::widths::load();
        let recent = crate::recent::load();
        let starred = crate::starred::load();
        let settings = crate::settings::load();
        crate::db::set_max_rows(settings.value.row_limit);
        let last = session::load();
        // Files that existed but couldn't be read were backed up; say so instead of starting
        // empty without a word.
        let mut warnings: Vec<String> = [
            conns.warning,
            secrets.take_warning(),
            history.warning,
            favorites.warning,
            filters.warning,
            col_widths.warning,
            settings.warning,
            last.warning,
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut app = Self {
            host: Arc::new(host),
            connections: Connections {
                saved: conns.list,
                secrets,
                master: None,
                form: None,
                conn_test: None,
                backup: None,
            },
            session: None,
            dialog: None,
            tabs: vec![],
            active: 0,
            next_id: 1,
            filter: String::new(),
            schema_focus: None,
            notice: None,
            fps: Fps {
                max: false,
                show: false,
                value: 0.0,
                frames: 0,
                since: Instant::now(),
            },
            library: Library {
                history: history.value,
                history_writer: history::Writer::new(),
                favorites: favorites.value,
                filters: filters.value,
                col_widths: col_widths.value,
                recent: recent.value,
                starred: starred.value,
            },
            nav_back: Vec::new(),
            restore: None,
            launch_files: Vec::new(),
            window_state: WindowState::default(),
            show_detail: true,
            sort_by_size: false,
            closed: Vec::new(),
            open_tab_ids: Vec::new(),
            split: None,
            settings: settings.value,
        };
        let vault_has_any = app.connections.secrets.locked_vault().is_some_and(|v| {
            app.connections
                .saved
                .iter()
                .any(|c| v.has(&c.id) || v.has(&c.name))
        });
        if conns.outdated {
            // Plain-text passwords or missing ids from older versions: rewrite the file now.
            app.save_connections();
        } else if vault_has_any {
            app.ask_master(AfterMaster::Nothing);
        }
        app.launch_files = launch.files;
        // What the command line asks for wins; else pick up where the last run stopped: same
        // connection, then its tabs (see `poll`).
        let last = last.value;
        let asked = launch.conn.as_deref().map(|name| {
            app.connections
                .saved
                .iter()
                .find(|c| c.id == name || c.name.eq_ignore_ascii_case(name))
                .map(|c| c.id.clone())
                .ok_or_else(|| format!("No saved connection named \"{name}\""))
        });
        match (launch.url, asked) {
            (Some(url), _) => {
                let mut cfg = ConnConfig::new(Kind::Postgres);
                match crate::url::apply(&mut cfg, &url) {
                    Ok(()) => {
                        cfg.name = if cfg.kind == Kind::Sqlite {
                            cfg.database.clone()
                        } else {
                            format!("{}/{}", cfg.host, cfg.database)
                        };
                        app.connect_cfg(cfg);
                    }
                    Err(e) => warnings.push(format!("Connection URL: {e}")),
                }
            }
            (None, Some(Err(e))) => warnings.push(e),
            (None, asked) => {
                let id = match asked {
                    Some(Ok(id)) => Some(id),
                    _ => last.conn.clone(),
                };
                if let Some(id) = id
                    && app.find_saved(&id).is_some()
                {
                    if last.conn.as_deref() == Some(id.as_str()) {
                        app.restore = Some(last);
                    }
                    app.connect_saved(&id);
                }
            }
        }
        #[cfg(feature = "dev-conn")]
        app.add_dev_connections();
        if !warnings.is_empty() {
            app.notice = Notice::error(warnings.join("\n"));
        }
        app
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
        dev_mysql.database = "shop".into();
        self.connections
            .saved
            .retain(|c| c.name != dev.name && c.name != dev_mysql.name);
        self.connections.saved.insert(0, dev_mysql);
        self.connections.saved.insert(0, dev);
        self.connect_saved("dev");
    }

    pub fn tick_fps(&mut self) {
        self.fps.frames += 1;
        let elapsed = self.fps.since.elapsed().as_secs_f32();
        if elapsed >= 0.5 {
            self.fps.value = self.fps.frames as f32 / elapsed;
            self.fps.frames = 0;
            self.fps.since = Instant::now();
        }
    }

    /// Stores the query from the "Save query" dialog.
    pub fn add_favorite(&mut self, p: FavPrompt) {
        let conn = if p.shared {
            String::new()
        } else {
            self.session
                .as_ref()
                .map(|s| s.cfg.id.clone())
                .unwrap_or_default()
        };
        self.library.favorites.push(Favorite {
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
        self.library
            .filters
            .retain(|f| !(f.conn == conn && f.schema == schema && f.table == table && f.name == name));
        self.library.filters.push(SavedFilter {
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
        self.library.filters.retain(|f| f.id != id);
        self.store_filters();
    }

    /// The widths remembered for a table of the open connection.
    #[cfg(test)]
    pub fn col_widths_for(&self, table: &str) -> std::collections::HashMap<String, f32> {
        let Some(s) = &self.session else {
            return Default::default();
        };
        let schema = s
            .tables
            .iter()
            .find(|t| t.name == table)
            .map_or("public", |t| t.schema.as_str());
        crate::widths::for_table(&self.library.col_widths, &s.cfg.id, schema, table)
    }

    /// Remembers column widths dragged in a table tab.
    pub fn remember_widths(&mut self, schema: &str, table: &str, widths: &[(String, f32)]) {
        let Some(conn) = self.session.as_ref().map(|s| s.cfg.id.clone()) else {
            return;
        };
        if crate::widths::remember(&mut self.library.col_widths, &conn, schema, table, widths)
            && let Err(e) = crate::widths::save(&self.library.col_widths)
        {
            self.notice = Notice::error(format!("Error saving column widths: {e}"));
        }
    }

    fn store_filters(&mut self) {
        if let Err(e) = filters::save(&self.library.filters) {
            self.notice = Notice::error(format!("Error saving filters: {e}"));
        }
    }

    pub fn delete_favorite(&mut self, id: &str) {
        self.library.favorites.retain(|f| f.id != id);
        self.save_favorites();
    }

    fn save_favorites(&mut self) {
        if let Err(e) = favorites::save(&self.library.favorites) {
            self.notice = Notice::error(format!("Error saving favorites: {e}"));
        }
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
        if self.host.focused() != Some(false) {
            return;
        }
        self.host.request_attention();
        self.host
            .set_title(&format!("✓ {title} finished ({} s) · tabella", took.as_secs()));
        self.window_state.title_changed = true;
    }

    /// Back in the window: the plain title again.
    pub fn clear_finished_notice(&mut self) {
        if self.window_state.title_changed && self.host.focused() == Some(true) {
            self.window_state.title_changed = false;
            self.host.set_title("tabella");
        }
    }

    /// The table was created or altered: refresh the sidebar and the tabs showing it, and swap
    /// the design tab for the new structure.
    fn after_design(&mut self, tab: u64, schema: &str, table: &str) {
        let Some(s) = &self.session else { return };
        s.send(Job::Tables);
        self.notice = Notice::info(format!("Structure of {table} applied"));
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
}

#[cfg(test)]
pub mod tests;

#[cfg(test)]
mod poll_tests;
