//! The state of dialogs and prompts over the window.

use super::*;

/// The one dialog over the window, if any: opening another replaces it. (The connection form
/// and the master-password prompt are separate: the latter opens over the former.)
pub enum Dialog {
    Palette(Palette),
    /// F1 shortcuts screen.
    Help,
    Favorite(FavPrompt),
    Path(PathPrompt),
    TableAction(TableActionPrompt),
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
    /// Wiped from memory when dropped, like `ConnConfig::password`.
    pub password: Secret,
    pub confirm: Secret,
    pub error: Option<String>,
    pub then: AfterMaster,
    /// Key derivation running off the UI thread (Argon2 takes a noticeable moment).
    pub working: Option<Receiver<Result<Derived, VaultError>>>,
}

/// "Save query" dialog.
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
    /// The settings tested (`ConnConfig::test_key`): a result says nothing once they change.
    pub key: Secret,
    pub rx: Receiver<Result<String, String>>,
    pub result: Option<Result<String, String>>,
}

/// What the foreign-key picker fills in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PickFor {
    /// A grid cell (row, column).
    Cell(usize, usize),
    /// A field of the "New row" form.
    InsertField(usize),
}

/// A referenced row in the picker: its key and its readable column.
pub type FkOption = (String, Option<String>);

/// "Escolher" a foreign key's value: the referenced rows, searchable, key and a readable column.
pub struct FkPicker {
    pub target: PickFor,
    /// The column being filled, and the row it points at.
    pub column: String,
    pub fk: crate::db::FkRef,
    pub label: Option<String>,
    pub numeric_key: bool,
    /// The whole referenced table is here (small): filtered locally.
    pub local: bool,
    pub search: String,
    /// The search the options came for; a different `search` asks again once typing pauses.
    pub asked: String,
    pub typed_at: Instant,
    pub loading: bool,
    pub options: Option<Result<Vec<FkOption>, String>>,
    pub current: Option<String>,
    pub focused: bool,
}

/// Opens the picker for a foreign-key column, from the session's catalog: the referenced
/// table's readable column and size decide what is shown and how it is searched.
pub fn open_fk_picker(
    tab: &mut Tab,
    s: &Session,
    target: PickFor,
    column: &str,
    fk: crate::db::FkRef,
    current: Option<String>,
) {
    let cols: Vec<(String, String)> = s
        .schema_cols
        .iter()
        .filter(|c| c.schema == fk.schema && c.table == fk.table)
        .map(|c| (c.column.clone(), c.data_type.clone()))
        .collect();
    let key_type = cols
        .iter()
        .find(|(c, _)| *c == fk.column)
        .map_or("", |(_, t)| t.as_str())
        .to_lowercase();
    let numeric_key = ["int", "serial", "numeric", "decimal"]
        .iter()
        .any(|n| key_type.contains(n));
    let label = sql::fk_label(&cols, &fk.column);
    let estimate = s
        .tables
        .iter()
        .find(|t| t.schema == fk.schema && t.name == fk.table)
        .and_then(|t| t.estimate);
    let local = estimate.is_some_and(|n| n <= sql::FK_LOCAL_ROWS);
    tab.fk_picker = Some(FkPicker {
        target,
        column: column.to_string(),
        fk,
        label,
        numeric_key,
        local,
        search: String::new(),
        asked: String::new(),
        typed_at: Instant::now(),
        loading: false,
        options: None,
        current,
        focused: false,
    });
    fk_search(tab, s);
}

/// Asks for the picker's options for what is typed now.
pub fn fk_search(tab: &mut Tab, s: &Session) {
    let Some(p) = &mut tab.fk_picker else { return };
    let search = if p.local {
        String::new()
    } else {
        p.search.trim().to_string()
    };
    let sql = sql::fk_options(
        s.cfg.kind,
        &p.fk,
        p.label.as_deref(),
        p.numeric_key,
        p.local,
        &search,
    );
    p.asked = p.search.clone();
    p.loading = true;
    let job = Job::FkOptions {
        tab: tab.id,
        sql,
        search,
    };
    tab.begin_job();
    s.send(job);
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

/// The "Set with an expression" form.
pub struct BulkPrompt {
    pub column: String,
    pub expression: String,
    /// The selected rows (else every row passing the filter).
    pub selected_only: bool,
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
    /// Open it in a "run file" tab instead of the editor.
    pub run: bool,
    /// What the file is for, when it isn't SQL for the editor.
    pub purpose: Option<PathUse>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PathUse {
    ExportConnections,
    ImportConnections,
    Backup,
    Restore,
}
