//! Opening, arranging and closing tabs and panes.

use super::*;

impl App {
    /// "New table" in the first schema of the connection (or the one given).
    pub fn open_new_table(&mut self, schema: Option<&str>) {
        let Some(s) = &self.session else { return };
        let schema = schema
            .map(str::to_string)
            .or_else(|| s.tables.first().map(|t| t.schema.clone()))
            .unwrap_or_else(|| match s.cfg.kind {
                Kind::Postgres => "public".to_string(),
                Kind::MySql => s.cfg.database.clone(),
                Kind::Sqlite => "main".to_string(),
            });
        let d = Design::new_table(s.cfg.kind, &schema);
        self.push_tab("new table".into(), TabKind::Design(Box::new(d)));
    }

    /// "Edit structure" of a table whose structure was loaded.
    pub fn open_design(&mut self, schema: &str, table: &str, data: &TableStructure) {
        let Some(s) = &self.session else { return };
        if let Some(i) = self.tabs.iter().position(
            |t| matches!(&t.kind, TabKind::Design(d) if d.existing && d.schema == schema && d.table == table),
        ) {
            self.active = i;
            return;
        }
        let d = Design::from_structure(s.cfg.kind, schema, table, data);
        self.push_tab(format!("{table} · edit"), TabKind::Design(Box::new(d)));
    }

    pub(super) fn push_tab(&mut self, title: String, kind: TabKind) -> usize {
        // A new tab opens in the pane the user is working in.
        let right = self.tabs.get(self.active).is_some_and(|t| t.right);
        let mut tab = Tab::new(self.next_id, title, kind);
        tab.right = right;
        self.tabs.push(tab);
        self.next_id += 1;
        self.active = self.tabs.len() - 1;
        self.active
    }

    /// Keeps the split view consistent with the tabs: the pane holding the active tab shows it,
    /// each pane falls back to one of its own tabs when its selected one is gone or moved, and a
    /// pane left without tabs ends the split.
    pub fn sync_split(&mut self) {
        let Some(sp) = &mut self.split else { return };
        let Some(cur) = self.tabs.get(self.active) else {
            self.split = None;
            return;
        };
        sp.shown[usize::from(cur.right)] = cur.id;
        for side in [false, true] {
            let shown = sp.shown[usize::from(side)];
            if self.tabs.iter().any(|t| t.id == shown && t.right == side) {
                continue;
            }
            match self.tabs.iter().rev().find(|t| t.right == side) {
                Some(t) => sp.shown[usize::from(side)] = t.id,
                None => {
                    self.close_split();
                    return;
                }
            }
        }
    }

    /// Back to one pane, keeping every tab.
    pub fn close_split(&mut self) {
        self.split = None;
        for t in &mut self.tabs {
            t.right = false;
        }
    }

    /// Moves tab `i` to the other pane and focuses it. Without a split, opens one: a SQL tab is
    /// copied to the new pane (so the original stays where it was), any other tab moves there.
    pub fn split_tab(&mut self, i: usize) {
        let Some(tab) = self.tabs.get(i) else { return };
        if self.split.is_some() {
            self.move_to_pane(i, !tab.right);
            return;
        }
        let id = tab.id;
        if tab.editor().is_some() {
            self.duplicate_tab(i);
        } else if self.tabs.len() > 1 {
            self.active = i;
        } else {
            return;
        }
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.right = true;
        }
        self.split = Some(Split { shown: [id, 0] });
        self.sync_split();
    }

    /// A tab dropped on the edge of the content: it goes alone to a new right pane (an editor
    /// that is the only tab is copied there instead).
    pub fn drag_to_new_pane(&mut self, i: usize) {
        if self.split.is_some() || i >= self.tabs.len() {
            return;
        }
        if self.tabs.len() < 2 {
            self.split_tab(i);
            return;
        }
        self.tabs[i].right = true;
        self.active = i;
        self.split = Some(Split { shown: [0, 0] });
        self.sync_split();
    }

    /// Moves tab `i` into a pane (and makes it the active one).
    pub fn move_to_pane(&mut self, i: usize, right: bool) {
        if self.split.is_none() {
            return;
        }
        if let Some(t) = self.tabs.get_mut(i) {
            t.right = right;
            self.active = i;
        }
        self.sync_split();
    }

    /// Pins or unpins a tab; pinned ones sit first in their strip.
    pub fn set_pinned(&mut self, i: usize, pinned: bool) {
        if let Some(t) = self.tabs.get_mut(i) {
            t.pinned = pinned;
            self.normalize_pins();
        }
    }

    /// Pinned tabs first within each pane, otherwise in the order they were. The active tab stays
    /// the same one.
    pub(super) fn normalize_pins(&mut self) {
        let mut order: Vec<usize> = (0..self.tabs.len()).collect();
        for right in [false, true] {
            let slots: Vec<usize> = self.pane_tabs(right).collect();
            // Stable: pinned (false after the negation) first, the rest keep their order.
            let mut members = slots.clone();
            members.sort_by_key(|&i| !self.tabs[i].pinned);
            for (slot, member) in slots.into_iter().zip(members) {
                order[slot] = member;
            }
        }
        if order.iter().enumerate().all(|(slot, &i)| slot == i) {
            return;
        }
        let active = self.tabs.get(self.active).map(|t| t.id);
        let mut old: Vec<Option<Tab>> = std::mem::take(&mut self.tabs).into_iter().map(Some).collect();
        self.tabs = order.iter().filter_map(|&i| old[i].take()).collect();
        if let Some(id) = active
            && let Some(i) = self.tabs.iter().position(|t| t.id == id)
        {
            self.active = i;
        }
    }

    /// Puts a tab (by id) before another one, or at the end when `before` is None, in the pane
    /// `pane` (None: the only strip). The tab becomes the active one.
    pub fn reorder_tab(&mut self, id: u64, before: Option<u64>, pane: Option<bool>) {
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        if before == Some(id) {
            self.active = from;
            return;
        }
        let mut tab = self.tabs.remove(from);
        if let (Some(r), true) = (pane, self.split.is_some()) {
            tab.right = r;
        }
        let at = before
            .and_then(|b| self.tabs.iter().position(|t| t.id == b))
            .unwrap_or_else(|| {
                // The end of its own pane.
                self.tabs
                    .iter()
                    .rposition(|t| t.right == tab.right)
                    .map_or(self.tabs.len(), |i| i + 1)
            });
        self.tabs.insert(at, tab);
        self.active = at;
        // Unpinned tabs can't be put before pinned ones, nor the other way round.
        self.normalize_pins();
        self.sync_split();
    }

    /// The tab ids of a pane, in strip order.
    pub fn pane_tabs(&self, right: bool) -> impl Iterator<Item = usize> + '_ {
        (0..self.tabs.len()).filter(move |&i| self.tabs[i].right == right)
    }

    pub fn close_tab(&mut self, i: usize) {
        if let Some(t) = self.tabs.get_mut(i)
            && file_dirty(t)
            && let Some(ed) = t.editor_mut()
            && !ed.close_armed
        {
            ed.close_armed = true;
            t.error = Some(format!(
                "{} has unsaved changes: Ctrl+S saves; close again to discard.",
                t.title
            ));
            self.active = i;
            return;
        }
        if let Some(t) = self.tabs.get_mut(i)
            && has_unsaved(t)
        {
            t.error =
                Some("This tab has unsaved changes: save (Ctrl+S) or discard them before closing.".into());
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
    pub fn end_tx(&self, commit: bool) {
        if let Some(s) = &self.session {
            s.send(Job::EndTx { commit });
        }
    }

    /// Switches manual-commit mode for this session; refused while a transaction is open.
    pub fn set_manual_commit(&mut self, on: bool) {
        let Some(s) = &mut self.session else { return };
        if s.txn.open && !on {
            self.notice =
                Notice::error("Commit or roll back the open transaction before going back to auto-commit.");
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
            format!("diagram · {schema}"),
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
        self.open_sql(format!("-- loading the definition of {}…", object.name));
        let Some(s) = &self.session else { return };
        let Some(tab) = self.tabs.last_mut() else { return };
        tab.title = object.name.clone();
        tab.renamed = true;
        tab.begin_job();
        s.send(Job::Source { tab: tab.id, object });
    }

    /// Runs the confirmed table action. Tabs showing a renamed or dropped table close (their
    /// queries would fail); an emptied one reloads.
    pub fn run_table_action(&mut self, p: &TableActionPrompt) {
        let Some(s) = &self.session else { return };
        let sql = sql::table_action(s.cfg.kind, &p.schema, &p.table, p.action, &p.input);
        let (done, failed) = match p.action {
            sql::TableAction::Rename => (
                format!("{} renamed to {}", p.table, p.input.trim()),
                format!("Couldn't rename {} to {}", p.table, p.input.trim()),
            ),
            sql::TableAction::Duplicate(_) => (
                format!("{} duplicated as {}", p.table, p.input.trim()),
                format!("Couldn't duplicate {} as {}", p.table, p.input.trim()),
            ),
            sql::TableAction::Truncate => (
                format!("{} truncated", p.table),
                format!("Couldn't empty {}", p.table),
            ),
            sql::TableAction::Drop => (
                format!("{} dropped", p.table),
                format!("Couldn't drop {}", p.table),
            ),
        };
        s.send(Job::Admin { sql, done, failed });
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

    /// "Run…" on a function or procedure: a new editor tab with the call, asking for the
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

    /// "Generate SQL": a statement for a table in a new editor tab.
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

    /// A `.sql` file in a "run file" tab (not run yet: the tab has the button).
    pub fn open_script(&mut self, path: &Path) {
        let Some(s) = &self.session else { return };
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let st = ScriptState {
            path: path.to_path_buf(),
            size,
            // Postgres undoes DDL too; MySQL commits on each DDL, so a transaction can't
            // promise "all or nothing" there.
            transaction: s.cfg.kind != Kind::MySql,
            running: false,
            progress: Default::default(),
            outcome: None,
            started: None,
            took: None,
            cancel: Arc::default(),
            slowest: Vec::new(),
        };
        self.push_tab(crate::files::title(path), TabKind::Script(Box::new(st)));
    }

    /// "Export .sql" of a table, a schema or the whole database, in a tab with its options.
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
                format!("table {schema}.{table}"),
                vec![(schema, table)],
                Vec::new(),
            ),
            DumpScope::Schema(schema) => (format!("schema {schema}"), all(Some(&schema)), vec![schema]),
            // MySQL lists every database in the sidebar; "the database" is the one in use.
            DumpScope::Database if s.cfg.kind == Kind::MySql => (
                format!("database {}", s.current_db),
                all(Some(&s.current_db)),
                vec![s.current_db.clone()],
            ),
            DumpScope::Database => {
                let tables = all(None);
                let mut schemas: Vec<String> = tables.iter().map(|(sc, _)| sc.clone()).collect();
                schemas.dedup();
                (format!("database {}", s.current_db), tables, schemas)
            }
        };
        let title = format!(
            "Export {}",
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

    /// "Compare structure": this connection's schema against another (here or elsewhere).
    pub fn open_compare(&mut self, schema: Option<String>) {
        let Some(s) = &self.session else { return };
        let here = match s.cfg.kind {
            Kind::Postgres => s.current_schema.clone(),
            Kind::MySql => s.current_db.clone(),
            Kind::Sqlite => "main".to_string(),
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
        self.push_tab("Compare structure".into(), TabKind::Compare(Box::new(st)));
    }

    /// Ctrl+P "Run .sql file…".
    pub fn pick_script(&mut self) {
        if crate::files::NATIVE_DIALOGS {
            if let Some(path) = crate::files::pick_open() {
                self.open_script(&path);
            }
        } else {
            self.dialog = Some(Dialog::Path(PathPrompt {
                save_tab: None,
                path: String::new(),
                run: true,
                purpose: None,
            }));
        }
    }
}

impl App {
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
        self.push_tab("History".into(), TabKind::History(Box::new(st)));
    }

    /// Drops one entry from the history (and the file).
    pub fn forget_history(&mut self, conn: &str, sql: &str) {
        history::forget(&mut self.library.history, conn, sql, &self.library.history_writer);
    }

    /// Opens the "search value" tab (or focuses it).
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
        self.push_tab("Search value".into(), TabKind::Search(Box::new(st)));
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
            every: Some(std::time::Duration::from_secs(2)),
            server_every: None,
            last_refresh: None,
            confirm_kill: None,
            kill_error: None,
            advice: None,
        };
        let i = self.push_tab("Activity".into(), TabKind::Activity(Box::new(state)));
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
        let from = self.tabs.get(self.active).map(|t| t.id);
        let Some(i) = existing else {
            self.open_table(schema, table);
            // Applied before the first page loads (that waits for the columns).
            let mut opened = None;
            if let Some(t) = self.tabs.last_mut()
                && let TabKind::Table(ts) = &mut t.kind
            {
                ts.filter = filter;
                opened = Some(t.id);
            }
            if let Some(to) = opened {
                self.remember_jump(from, to, None);
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
                    Some("This tab has pending changes: save or discard them before filtering.".into());
                return;
            }
            let previous = std::mem::replace(&mut ts.filter, filter);
            ts.search.clear();
            ts.page = 0;
            let to = tab.id;
            reload(tab, s);
            self.remember_jump(from, to, Some(previous));
            return;
        }
        reload(tab, s);
    }

    pub(super) fn remember_jump(&mut self, from: Option<u64>, to: u64, previous_filter: Option<String>) {
        const KEEP: usize = 50;
        if let Some(from) = from {
            self.nav_back.push(NavStep {
                from,
                to,
                previous_filter,
            });
            if self.nav_back.len() > KEEP {
                self.nav_back.remove(0);
            }
        }
    }

    pub fn can_go_back(&self) -> bool {
        !self.nav_back.is_empty()
    }

    /// Undoes the last jump: back to the tab it started from, and the table it filtered gets its
    /// earlier filter back. Steps whose tab was closed are skipped.
    pub fn go_back(&mut self) {
        while let Some(step) = self.nav_back.pop() {
            let Some(origin) = self.tabs.iter().position(|t| t.id == step.from) else {
                continue;
            };
            if let (Some(filter), Some(s)) = (step.previous_filter, &self.session)
                && let Some(tab) = self.tabs.iter_mut().find(|t| t.id == step.to)
                && let TabKind::Table(ts) = &mut tab.kind
                && ts.changes.is_empty()
            {
                ts.filter = filter;
                ts.search.clear();
                ts.page = 0;
                reload(tab, s);
            }
            self.active = origin;
            return;
        }
    }

    /// Where the table is among the last opened on this connection (0 = the latest).
    pub fn recent_rank(&self, schema: &str, table: &str) -> Option<usize> {
        let s = self.session.as_ref()?;
        crate::recent::rank(&self.library.recent, &s.cfg.id, schema, table)
    }

    /// Opens a generated UPDATE in a new editor and shows what it would change; running it is
    /// the editor's own "Run" afterwards.
    pub fn open_update_preview(&mut self, sql: String) {
        self.open_sql(sql.clone());
        // The preview may run the statement (rolled back): only one UPDATE or DELETE, like
        // the editor's own Preview.
        if !previewable(&sql) {
            return;
        }
        if let (Some(s), Some(tab)) = (&self.session, self.tabs.last_mut()) {
            preview(tab, s, sql);
        }
    }

    /// Stars or unstars a table in the sidebar.
    pub fn toggle_star(&mut self, schema: &str, table: &str) {
        let Some(s) = &self.session else { return };
        crate::starred::toggle(&mut self.library.starred, &s.cfg.id, schema, table);
        if let Err(e) = crate::starred::save(&mut self.library.starred) {
            self.notice = Notice::error(format!("Error saving starred tables: {e}"));
        }
    }

    pub(super) fn remember_recent(&mut self, schema: &str, table: &str) {
        let Some(s) = &self.session else { return };
        if crate::recent::touch(&mut self.library.recent, &s.cfg.id, schema, table)
            && let Err(e) = crate::recent::save(&mut self.library.recent)
        {
            self.notice = Notice::error(format!("Error saving recent tables: {e}"));
        }
    }

    pub fn open_table(&mut self, schema: &str, table: &str) {
        self.remember_recent(schema, table);
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
        let widths = crate::widths::for_table(&self.library.col_widths, &s.cfg.id, schema, table);
        let i = self.push_tab(
            table.into(),
            TabKind::Table(Box::new(TableState::new(schema, table, estimate))),
        );
        self.tabs[i].col_widths = widths;
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
        self.open_structure_at(schema, table, None);
    }

    /// The structure of a table, pointing at `column` when given (the tab is reused if open).
    pub fn open_structure_at(&mut self, schema: &str, table: &str, column: Option<&str>) {
        if self.session.is_none() {
            return;
        }
        let existing = self.tabs.iter().position(
            |t| matches!(&t.kind, TabKind::Structure(st) if st.schema == schema && st.table == table),
        );
        if let Some(i) = existing {
            self.active = i;
            if let TabKind::Structure(st) = &mut self.tabs[i].kind
                && let Some(c) = column
            {
                st.focus = Some(c.to_string());
                st.focus_seq += 1;
            }
            return;
        }
        let st = StructState {
            schema: schema.into(),
            table: table.into(),
            data: None,
            focus: column.map(str::to_string),
            focus_seq: 1,
        };
        let i = self.push_tab(format!("{table} · structure"), TabKind::Structure(Box::new(st)));
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
