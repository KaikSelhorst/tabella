//! The session file, SQL files on disk and closed tabs.

use super::*;

impl App {
    /// What to restore next time: open connection, tabs and window size.
    pub fn snapshot(&self) -> session::Saved {
        // Only some kinds of tab are saved: the split refers to them by their place in the list.
        let kept: Vec<(&Tab, SavedTab)> = self
            .tabs
            .iter()
            .filter_map(|t| saved_tab(t).map(|s| (t, s)))
            .collect();
        let split = self.split.as_ref().map(|sp| session::SavedSplit {
            right: kept.iter().map(|(t, _)| t.right).collect(),
            shown: sp
                .shown
                .map(|id| kept.iter().position(|(t, _)| t.id == id).unwrap_or(0)),
        });
        // A saved list that lost some tabs can't say which one was active.
        let active = self
            .tabs
            .get(self.active)
            .and_then(|a| kept.iter().position(|(t, _)| t.id == a.id))
            .unwrap_or(0);
        let in_use = |s: &Session| match s.cfg.kind {
            Kind::Postgres => Some(s.cfg.database.clone()),
            Kind::MySql => Some(s.current_db.clone()),
            Kind::Sqlite => None,
        };
        let database = self.session.as_ref().and_then(in_use).filter(|d| !d.is_empty());
        let schema = self
            .session
            .as_ref()
            .filter(|s| s.cfg.kind == Kind::Postgres)
            .map(|s| s.current_schema.clone())
            .filter(|c| !c.is_empty());
        session::Saved {
            conn: self.session.as_ref().map(|s| s.cfg.id.clone()),
            database,
            schema,
            pinned: kept.iter().map(|(t, _)| t.pinned).collect(),
            tabs: kept.into_iter().map(|(_, s)| s).collect(),
            active,
            window: self.window,
            split,
        }
    }

    pub(super) fn restore_tabs(&mut self, saved: session::Saved) {
        // Postgres reconnected to the database it was in (see `connect_saved_now`); the rest
        // of the location is session state of the new connection. Before the tabs' queries.
        if let Some(s) = &self.session {
            match (s.cfg.kind, &saved.database, &saved.schema) {
                (Kind::MySql, Some(db), _) if *db != s.current_db => s.send(Job::UseSchema(db.clone())),
                (Kind::Postgres, _, Some(sc)) if sc != "public" => s.send(Job::UseSchema(sc.clone())),
                _ => {}
            }
        }
        let count = saved.tabs.len();
        for t in saved.tabs {
            self.restore_tab(t);
        }
        self.active = saved.active.min(self.tabs.len().saturating_sub(1));
        // Only when every tab came back as its own: otherwise the indices mean something else.
        if let Some(sp) = saved.split
            && self.tabs.len() == count
            && sp.right.len() == count
            && sp.shown.iter().all(|&i| i < count)
        {
            for (tab, right) in self.tabs.iter_mut().zip(&sp.right) {
                tab.right = *right;
            }
            self.split = Some(Split {
                shown: sp.shown.map(|i| self.tabs[i].id),
            });
            self.sync_split();
        }
        // Pins go by position too, so only when every tab came back.
        if self.tabs.len() == count && saved.pinned.len() == count {
            let active = self.tabs.get(self.active).map(|t| t.id);
            for (tab, pinned) in self.tabs.iter_mut().zip(&saved.pinned) {
                tab.pinned = *pinned;
            }
            self.normalize_pins();
            if let Some(i) = active.and_then(|id| self.tabs.iter().position(|t| t.id == id)) {
                self.active = i;
            }
        }
    }

    /// Opens a tab as it was saved (session restore, "reopen closed tab").
    pub(super) fn restore_tab(&mut self, t: SavedTab) {
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
                    then_by,
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
                            ts.order = order.into_iter().chain(then_by).collect();
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
    pub fn save_file_as(&mut self, i: usize, path: &Path) {
        let Some(t) = self.tabs.get_mut(i) else { return };
        let TabKind::Sql(ed) = &mut t.kind else { return };
        match crate::files::write_sql(path, &ed.text) {
            Ok(written) => {
                ed.saved_text = ed.text.clone();
                t.title = crate::files::title(&written);
                self.notice = Notice::info(format!("Saved to {}", written.display()));
                ed.file = Some(written);
                ed.close_armed = false;
            }
            Err(e) => self.notice = Notice::error(e),
        }
    }

    /// Ctrl+S on an editor tab: its file, or "save as" when it has none yet.
    pub fn save_file(&mut self, i: usize, ask: bool) {
        let Some(t) = self.tabs.get(i) else { return };
        match (t.editor().and_then(|ed| ed.file.as_ref()), ask) {
            (Some(path), false) => {
                let path = path.clone();
                self.save_file_as(i, &path);
            }
            _ => {
                let suggested = format!("{}.sql", t.title.trim_end_matches(".sql"));
                if crate::files::NATIVE_DIALOGS {
                    if let Some(path) = crate::files::pick_save(&suggested) {
                        self.save_file_as(i, &path);
                    }
                } else {
                    self.dialog = Some(Dialog::Path(PathPrompt {
                        save_tab: Some(t.id),
                        path: suggested,
                        run: false,
                        purpose: None,
                    }));
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
            self.dialog = Some(Dialog::Path(PathPrompt {
                save_tab: None,
                path: String::new(),
                run: false,
                purpose: None,
            }));
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
        let (text, title) = (ed.text.clone(), format!("{} (copy)", t.title));
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
            .filter(|(i, t)| *i != keep && !t.pinned && (!only_right || *i > keep))
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
}

/// An editor tab whose text differs from its file.
pub fn file_dirty(t: &Tab) -> bool {
    t.editor()
        .is_some_and(|ed| ed.file.is_some() && ed.text != ed.saved_text)
}

/// Closed tabs remembered for "reopen closed tab".
pub(super) const MAX_CLOSED: usize = 20;

/// A tab as the session file (and the closed-tabs list) keeps it; `None` for tabs that are
/// only meaningful while open.
pub(super) fn saved_tab(t: &Tab) -> Option<SavedTab> {
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
            order: ts.order.first().cloned(),
            then_by: ts.order.iter().skip(1).cloned().collect(),
            hidden: t.hidden_cols.clone(),
            pinned: t.pinned_cols.clone(),
        }),
        TabKind::Structure(st) => Some(SavedTab::Structure {
            schema: st.schema.clone(),
            table: st.table.clone(),
        }),
    }
}
