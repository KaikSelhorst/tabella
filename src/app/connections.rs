//! Saved connections, the master password, connecting, and backups.

use super::*;

impl App {
    pub fn find_saved(&self, id: &str) -> Option<&ConnConfig> {
        self.connections.saved.iter().find(|c| c.id == id)
    }

    /// Saves, first asking for the master password if passwords would otherwise be lost.
    pub fn save_connections(&mut self) {
        let has_passwords = self.connections.saved.iter().any(|c| !c.password.is_empty());
        if has_passwords && self.connections.secrets.locked_vault().is_some() {
            self.ask_master(AfterMaster::Save);
        } else {
            self.write_connections();
        }
    }

    pub(super) fn write_connections(&mut self) {
        match config::save(&self.connections.saved, &mut self.connections.secrets) {
            Ok(r) if r.not_stored > 0 => {
                self.notice = Notice::info(format!(
                    "{} password(s) weren't saved to disk (this session only)",
                    r.not_stored
                ))
            }
            Ok(r) if r.not_removed > 0 => {
                self.notice = Notice::error(format!(
                    "{} old password(s) couldn't be deleted from: {}",
                    r.not_removed,
                    self.connections.secrets.describe()
                ))
            }
            Ok(_) => {}
            Err(e) => self.notice = Notice::error(format!("Error saving connections: {e}")),
        }
    }

    /// Adds or replaces (by id) a connection from the form and saves the list.
    pub fn store_connection(&mut self, mut cfg: ConnConfig) {
        match self
            .connections
            .saved
            .iter()
            .position(|c| c.id == cfg.id && !cfg.id.is_empty())
        {
            Some(i) => self.connections.saved[i] = cfg.clone(),
            None => {
                cfg.id = config::new_id();
                self.connections.saved.push(cfg.clone());
            }
        }
        self.save_connections();
        // Editing the open connection: what can change live does, now.
        if let Some(s) = &mut self.session
            && s.cfg.id == cfg.id
            && !apply_live_settings(s, &cfg)
        {
            self.notice = Notice::info(
                "Saved. Reconnect (menu … → Reconnect) to apply server, user, database, TLS, \
                 read-only, timeout or SSH.",
            );
        }
    }

    pub fn delete_connection(&mut self, id: &str) {
        let Some(i) = self.connections.saved.iter().position(|c| c.id == id) else {
            return;
        };
        let removed = self.connections.saved.remove(i);
        if self.session.as_ref().is_some_and(|s| s.cfg.id == removed.id) {
            self.disconnect();
        }
        let forgot = config::forget(&removed, &mut self.connections.secrets);
        self.save_connections();
        if forgot.is_err() {
            self.notice = Notice::error(format!(
                "The password of “{}” couldn't be deleted from: {}",
                removed.name,
                self.connections.secrets.describe()
            ));
        }
    }

    /// Starts deriving the vault key from the typed master password, off the UI thread
    /// (Argon2 is slow on purpose); `finish_master_kdf` applies it.
    pub fn start_master_kdf(&mut self) {
        let wake = self.waker();
        let Some(p) = &mut self.connections.master else {
            return;
        };
        let create = p.mode == MasterMode::Create;
        if create && p.password != p.confirm {
            p.error = Some("The passwords don't match".into());
            return;
        }
        let Some(vault) = self.connections.secrets.vault_mut() else {
            return;
        };
        match vault.kdf_job(&p.password, create) {
            Ok(job) => {
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(job.run());
                    wake();
                });
                p.error = None;
                p.working = Some(rx);
            }
            Err(e) => p.error = Some(e),
        }
    }

    /// Applies a finished key derivation (or shows why it failed).
    pub fn finish_master_kdf(&mut self) {
        let Some(p) = &mut self.connections.master else {
            return;
        };
        let Some(rx) = &p.working else { return };
        let Ok(result) = rx.try_recv() else { return };
        p.working = None;
        let applied = match (result, self.connections.secrets.vault_mut()) {
            (Ok(derived), Some(vault)) => vault.finish(derived),
            (Err(e), _) => Err(e),
            (Ok(_), None) => Ok(()),
        };
        match applied {
            Ok(()) => {
                let then = p.then.clone();
                self.connections.master = None;
                self.notice = Notice::info(format!(
                    "Passwords protected by the {}",
                    self.connections.secrets.describe()
                ));
                self.after_master(then);
            }
            Err(e) => {
                p.error = Some(e);
                p.password.clear();
                p.confirm.clear();
            }
        }
    }

    /// The form's connection test: a finished one arrives, and one run with other settings
    /// than the form has now is dropped once it is done.
    pub fn poll_conn_test(&mut self) {
        let Some(form) = &self.connections.form else {
            return;
        };
        let key = form.cfg.test_key();
        if let Some(test) = &mut self.connections.conn_test {
            if test.key != key && test.result.is_some() {
                self.connections.conn_test = None;
            } else if let Ok(r) = test.rx.try_recv() {
                test.result = Some(r);
            }
        }
    }

    /// "Test connection": connects with the form's settings without saving them.
    pub fn start_conn_test(&mut self) {
        let Some(form) = &self.connections.form else {
            return;
        };
        self.connections.conn_test = Some(ConnTest {
            key: form.cfg.test_key(),
            rx: worker::test_connection(form.cfg.clone(), self.waker()),
            result: None,
        });
    }

    /// Leaves the connection form, saving what it holds or not.
    pub fn close_connection_form(&mut self, save: bool) {
        self.connections.conn_test = None;
        if let Some(mut form) = self.connections.form.take()
            && save
        {
            form.cfg.name = form.cfg.name.trim().to_string();
            self.store_connection(form.cfg);
        }
    }

    pub fn ask_master(&mut self, then: AfterMaster) {
        let Some(v) = self.connections.secrets.locked_vault() else {
            return self.after_master(then);
        };
        let mode = if v.exists() {
            MasterMode::Unlock
        } else {
            MasterMode::Create
        };
        self.connections.master = Some(MasterPrompt {
            mode,
            password: Secret::default(),
            confirm: Secret::default(),
            error: None,
            then,
            working: None,
        });
    }

    /// Runs after the vault was opened or the prompt was skipped.
    pub fn after_master(&mut self, then: AfterMaster) {
        config::fill_passwords(&mut self.connections.saved, &self.connections.secrets);
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
                .connections
                .secrets
                .locked_vault()
                .is_some_and(|v| v.has(&c.id) || v.has(&c.name));
        if needs_vault {
            self.ask_master(AfterMaster::Connect(id.to_string()));
        } else {
            self.connect_saved_now(id);
        }
    }

    pub(super) fn connect_saved_now(&mut self, id: &str) {
        let Some(mut cfg) = self.find_saved(id).cloned() else {
            return;
        };
        // Resuming a session: the Postgres database it was in, not the one the connection was
        // saved with. Once: if that fails, the next try uses the saved one.
        if cfg.kind == Kind::Postgres
            && let Some(r) = self.restore.as_mut().filter(|r| r.conn.as_deref() == Some(id))
            && let Some(db) = r.database.take()
        {
            cfg.database = db;
        }
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

    pub(super) fn connect_cfg(&mut self, cfg: ConnConfig) {
        // Refused with unsaved changes or an open transaction: stay on the current connection.
        if !self.disconnect() {
            return;
        }
        let cfg_manual = cfg.manual_commit;
        let worker::Spawned {
            jobs: tx,
            replies: rx,
            running,
            cancels,
        } = worker::spawn(cfg.clone(), self.waker());
        self.notice = Notice::info(format!("Connecting to {}…", cfg.name));
        self.session = Some(Session {
            cfg,
            tx,
            rx,
            running,
            cancels,
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
            uncommitted: HashSet::new(),
        });
    }

    /// Closes the connection. Refused (returns false) while a tab has unsaved changes.
    pub fn disconnect(&mut self) -> bool {
        if self.session.as_ref().is_some_and(|s| s.txn.open) {
            self.notice = Notice::error(
                "There's an open transaction: commit or roll back before switching connections.",
            );
            return false;
        }
        if let Some(i) = self.tabs.iter().position(has_unsaved) {
            self.active = i;
            self.notice = Notice::error(
                "There are unsaved changes: save or discard them before switching connections.",
            );
            return false;
        }
        if let Some(s) = self.session.take() {
            // Don't leave a long query running on the server after we stop listening.
            if self.tabs.iter().any(Tab::busy) {
                s.cancel_all();
            }
            self.notice = Notice::info("Disconnected");
        }
        self.tabs.clear();
        true
    }
}

impl App {
    /// "Export connections": a JSON file without passwords, to share or back up.
    pub fn export_connections_dialog(&mut self) {
        if self.connections.saved.is_empty() {
            self.notice = Notice::error("There are no saved connections to export");
        } else if crate::files::NATIVE_DIALOGS {
            if let Some(path) = crate::files::pick_connections_save() {
                self.export_connections(&path);
            }
        } else {
            self.dialog = Some(Dialog::Path(PathPrompt {
                save_tab: None,
                path: "tabella-connections.json".into(),
                run: false,
                purpose: Some(PathUse::ExportConnections),
            }));
        }
    }

    pub fn import_connections_dialog(&mut self) {
        if crate::files::NATIVE_DIALOGS {
            if let Some(path) = crate::files::pick_connections() {
                self.import_connections(&path);
            }
        } else {
            self.dialog = Some(Dialog::Path(PathPrompt {
                save_tab: None,
                path: String::new(),
                run: false,
                purpose: Some(PathUse::ImportConnections),
            }));
        }
    }

    pub fn export_connections(&mut self, path: &std::path::Path) {
        self.notice = match config::export_to(path, &self.connections.saved) {
            Ok(n) => Notice::info(format!(
                "{n} connection(s) exported to {} (without passwords)",
                path.display()
            )),
            Err(e) => Notice::error(e),
        };
    }

    pub fn import_connections(&mut self, path: &std::path::Path) {
        match config::import_from(path, &self.connections.saved) {
            Ok(list) if list.is_empty() => self.notice = Notice::error("The file has no connections"),
            Ok(list) => {
                let n = list.len();
                self.connections.saved.extend(list);
                self.write_connections();
                self.notice = Notice::info(format!(
                    "{n} connection(s) imported: open each to enter its password"
                ));
            }
            Err(e) => self.notice = Notice::error(e),
        }
    }

    /// "Backup" / "Restore" with the database's own tools; asks for the file first.
    pub fn backup_dialog(&mut self, restore: bool) {
        let Some(s) = self.session.as_ref().filter(|s| s.ready) else {
            return;
        };
        if s.cfg.kind == Kind::Sqlite {
            self.notice = Notice::error("SQLite is a single file: copy it instead");
            return;
        }
        let ext = if s.cfg.kind == Kind::Postgres {
            "dump"
        } else {
            "sql"
        };
        let (purpose, path) = if restore {
            (PathUse::Restore, String::new())
        } else {
            (PathUse::Backup, format!("{}.{ext}", s.cfg.database))
        };
        if crate::files::NATIVE_DIALOGS {
            let picked = if restore {
                crate::files::pick_backup()
            } else {
                crate::files::pick_backup_save(&path)
            };
            if let Some(file) = picked {
                self.start_backup(&file, restore);
            }
        } else {
            self.dialog = Some(Dialog::Path(PathPrompt {
                save_tab: None,
                path,
                run: false,
                purpose: Some(purpose),
            }));
        }
    }

    /// Runs the tool on a thread; the outcome arrives through `poll_backup`.
    pub fn start_backup(&mut self, file: &std::path::Path, restore: bool) {
        let Some(s) = &self.session else { return };
        if self.connections.backup.is_some() {
            self.notice = Notice::error("A backup or restore is already running");
            return;
        }
        let (cfg, file) = (s.cfg.clone(), file.to_path_buf());
        let op = if restore {
            crate::backup::Op::Restore
        } else {
            crate::backup::Op::Backup
        };
        self.notice = Notice::info(format!(
            "{} {}…",
            if restore { "Restoring" } else { "Backing up to" },
            file.display()
        ));
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = self.waker();
        std::thread::spawn(move || {
            let _ = tx.send(crate::backup::run(&cfg, op, &file));
            wake();
        });
        self.connections.backup = Some(rx);
    }

    /// Reports a finished backup or restore. After a restore the tables may be different.
    pub fn poll_backup(&mut self) {
        let Some(rx) = &self.connections.backup else {
            return;
        };
        let done = match rx.try_recv() {
            Ok(res) => res,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("the backup tool stopped unexpectedly".into())
            }
        };
        self.connections.backup = None;
        self.notice = match done {
            Ok(msg) => Notice::info(msg),
            Err(e) => Notice::error(e),
        };
    }
}

/// A new, empty SQLite database at `path`.
pub fn create_database_file(path: &Path) -> crate::error::DbResult<()> {
    crate::sqlite::create(&path.display().to_string())
}
