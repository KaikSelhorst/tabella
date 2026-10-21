use eframe::egui::{self, TextEdit};

use super::{icon, palette, theme};
use crate::app::{App, ConnForm, Dialog, DumpScope, Session, TabKind, TableActionPrompt};
use crate::db::{ConnConfig, DbObject, Kind, ObjKind, TableRef};
use crate::sql::{TableAction, Template};

/// Schemas with fewer tables than this start expanded.
const AUTO_EXPAND_BELOW: usize = 200;

impl App {
    pub fn side_panel(&mut self, ui: &mut egui::Ui) {
        self.connection_list(ui);
        self.location_row(ui);
        ui.separator();
        self.table_tree(ui);
    }

    /// Where unqualified names in the editor go: the database (MySQL `USE`, or another
    /// Postgres database through a reconnect) and, on Postgres, the schema.
    fn location_row(&mut self, ui: &mut egui::Ui) {
        let Some(s) = self
            .session
            .as_ref()
            .filter(|s| s.ready && !s.current_db.is_empty())
        else {
            return;
        };
        let tk = theme::tokens();
        // A SQLite file is the whole database: which file, nothing to switch.
        if s.cfg.kind == Kind::Sqlite {
            ui.add_space(4.0);
            let file = egui::Label::new(theme::icon_text(
                icon::FILE,
                &format!(" {}", s.cfg.database),
                tk.muted_foreground,
                tk.muted_foreground,
            ))
            .truncate();
            ui.add(file).on_hover_text(&s.cfg.database);
            return;
        }
        let pg = s.cfg.kind == Kind::Postgres;
        let mut schemas: Vec<String> = s.tables.iter().map(|t| t.schema.clone()).collect();
        schemas.dedup();
        if !schemas.contains(&s.current_schema) && !s.current_schema.is_empty() {
            schemas.insert(0, s.current_schema.clone());
        }
        let (dbs, db, schema) = (
            s.databases.clone(),
            s.current_db.clone(),
            s.current_schema.clone(),
        );
        let mut pick_db: Option<String> = None;
        let mut pick_schema: Option<String> = None;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let combo =
                |ui: &mut egui::Ui, id: &str, current: &str, items: &[String], width: f32, tip: &str| {
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt(id)
                        .selected_text(egui::RichText::new(current).monospace().size(12.0))
                        .width(width)
                        .show_ui(ui, |ui| {
                            for it in items {
                                if ui
                                    .selectable_label(it == current, egui::RichText::new(it).monospace())
                                    .clicked()
                                    && it != current
                                {
                                    chosen = Some(it.clone());
                                }
                            }
                        })
                        .response
                        .on_hover_text(tip);
                    chosen
                };
            ui.label(theme::icon(icon::DATABASE).size(12.0).color(tk.subtle));
            let half = (ui.available_width() - 28.0) / 2.0;
            let db_tip = if pg {
                "Database: switching reconnects to the same server (this connection's tabs close)"
            } else {
                "Database where unprefixed names are looked up (USE)"
            };
            pick_db = combo(
                ui,
                "loc_db",
                &db,
                &dbs,
                if pg { half } else { ui.available_width() - 8.0 },
                db_tip,
            );
            if pg {
                ui.label(egui::RichText::new(".").color(tk.subtle));
                pick_schema = combo(
                    ui,
                    "loc_schema",
                    &schema,
                    &schemas,
                    ui.available_width() - 8.0,
                    "Schema where unprefixed names are looked up first (search_path)",
                );
            }
        });
        if let Some(db) = pick_db {
            self.switch_location(Some(&db), None);
        }
        if let Some(sc) = pick_schema {
            self.switch_location(None, Some(&sc));
        }
    }

    fn connection_list(&mut self, ui: &mut egui::Ui) {
        let current = self
            .session
            .as_ref()
            .and_then(|s| self.saved.iter().position(|c| c.id == s.cfg.id));
        let mut connect = None;
        let mut edit = None;
        let mut delete = None;
        let mut add = false;
        let mut manual: Option<bool> = None;
        let session_manual = self.session.as_ref().map(|s| s.txn.manual);

        // Right to left inside one row: the menu button first, then the combo box fills what is
        // left. (Without the row, the layout would center itself in the whole panel height.)
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let selected: egui::WidgetText = match current {
                    // The label follows this session's mode, which can differ from the saved one.
                    Some(i) => conn_label(&ConnConfig {
                        manual_commit: session_manual.unwrap_or(self.saved[i].manual_commit),
                        ..self.saved[i].clone()
                    })
                    .into(),
                    None => egui::RichText::new("Select connection…")
                        .color(theme::tokens().muted_foreground)
                        .into(),
                };
                ui.add_enabled_ui(current.is_some(), |ui| {
                    ui.menu_button(theme::icon(icon::DOTS_THREE), |ui| {
                        if ui.button("Reconnect").clicked() {
                            connect = current;
                            ui.close();
                        }
                        if ui.button("Edit").clicked() {
                            edit = current;
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            delete = current;
                            ui.close();
                        }
                        if let Some(mut on) = session_manual {
                            ui.separator();
                            if ui
                                .checkbox(&mut on, "Manual commit in this session")
                                .on_hover_text("Changes stay in a transaction until you commit or roll back")
                                .changed()
                            {
                                manual = Some(on);
                                ui.close();
                            }
                        }
                    });
                });
                egui::ComboBox::from_id_salt("connection")
                    .selected_text(selected)
                    // Exactly the space left after the menu button: a guessed width made the row a
                    // few pixels wider than the panel, and the panel grew by that much every frame.
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for (group, indexes) in crate::db::by_group(&self.saved) {
                            if !group.is_empty() {
                                ui.label(egui::RichText::new(group).small().color(theme::tokens().subtle));
                            }
                            for i in indexes {
                                let c = &self.saved[i];
                                let r = ui.selectable_label(current == Some(i), conn_label(c));
                                if r.clicked() {
                                    connect = Some(i);
                                    ui.close();
                                }
                                r.context_menu(|ui| {
                                    if ui.button("Edit").clicked() {
                                        edit = Some(i);
                                        ui.close();
                                    }
                                    if ui.button("Delete").clicked() {
                                        delete = Some(i);
                                        ui.close();
                                    }
                                });
                            }
                        }
                        if !self.saved.is_empty() {
                            ui.separator();
                        }
                        if ui
                            .selectable_label(
                                false,
                                theme::icon_text(
                                    icon::PLUS,
                                    "Add connection…",
                                    theme::tokens().lime,
                                    theme::tokens().foreground,
                                ),
                            )
                            .clicked()
                        {
                            add = true;
                            ui.close();
                        }
                    });
            });
        });

        // Indices are only valid within this frame; from here on connections go by id.
        if add {
            self.form = Some(ConnForm {
                editing: None,
                cfg: ConnConfig::new(Kind::Postgres),
            });
        }
        if let Some(id) = connect.map(|i| self.saved[i].id.clone()) {
            self.connect_saved(&id);
        }
        if let Some(c) = edit.map(|i| self.saved[i].clone()) {
            self.form = Some(ConnForm {
                editing: Some(c.id.clone()),
                cfg: c,
            });
        }
        if let Some(id) = delete.map(|i| self.saved[i].id.clone()) {
            self.delete_connection(&id);
        }
        if let Some(on) = manual {
            self.set_manual_commit(on);
        }
    }

    fn table_tree(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.session else {
            ui.label(
                egui::RichText::new("No active connection")
                    .size(11.0)
                    .color(theme::tokens().subtle),
            );
            return;
        };
        if !s.ready {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Connecting…");
            });
            return;
        }
        let mut action = tree_toolbar(ui, s, self.sort_by_size);
        ui.add(
            TextEdit::singleline(&mut self.filter)
                .hint_text("Filter tables…")
                .margin(egui::Margin::symmetric(8, 5))
                .desired_width(ui.available_width()),
        );
        let active_table = self.tabs.get(self.active).and_then(|t| match &t.kind {
            TabKind::Table(ts) => Some((ts.schema.as_str(), ts.table.as_str())),
            TabKind::Structure(st) => Some((st.schema.as_str(), st.table.as_str())),
            TabKind::Sql(_)
            | TabKind::Activity(_)
            | TabKind::Design(_)
            | TabKind::Search(_)
            | TabKind::Diagram(_)
            | TabKind::History(_)
            | TabKind::Script(_)
            | TabKind::Dump(_)
            | TabKind::Compare(_) => None,
        });
        let starred = self.starred.get(&s.cfg.id).cloned().unwrap_or_default();
        let tree = Tree {
            s,
            needle: self.filter.to_lowercase(),
            by_size: self.sort_by_size,
            active_table,
            starred: &starred,
        };
        let focus = self.schema_focus.take();
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.spacing_mut().button_padding = egui::vec2(6.0, 2.0);
                if let Some(a) = tree.starred_section(ui) {
                    action = Some(a);
                }
                // Where unqualified names go (the Postgres schema, the MySQL database) comes
                // first and is marked.
                let mut groups: Vec<&[TableRef]> = s.tables.chunk_by(|a, b| a.schema == b.schema).collect();
                groups.sort_by_key(|g| g[0].schema != tree.default_schema());
                for group in groups {
                    if let Some(a) = tree.schema_section(ui, group, focus.as_deref()) {
                        action = Some(a);
                    }
                }
            });
        if let Some(a) = action {
            self.apply_tree_action(a);
        }
    }

    fn apply_tree_action(&mut self, action: TreeAction) {
        match action {
            TreeAction::Disconnect => {
                self.disconnect();
            }
            TreeAction::Activity => self.open_activity(),
            TreeAction::NewTable(schema) => self.open_new_table(schema.as_deref()),
            TreeAction::Diagram(schema) => self.open_diagram(Some(&schema)),
            TreeAction::ToggleSort => self.sort_by_size = !self.sort_by_size,
            TreeAction::NewSql => self.open_sql(String::new()),
            TreeAction::Open(schema, table) => self.open_table(&schema, &table),
            TreeAction::ToggleStar(schema, table) => self.toggle_star(&schema, &table),
            TreeAction::Structure(schema, table) => self.open_structure(&schema, &table),
            TreeAction::Sql(sql) => self.open_sql(sql),
            TreeAction::Template(schema, table, what) => self.open_template(&schema, &table, what),
            TreeAction::Object(o) => self.open_object(o),
            TreeAction::Run(o) => self.run_object(&o),
            TreeAction::TableAction(p) => self.dialog = Some(Dialog::TableAction(p)),
            TreeAction::Dump(scope) => self.open_dump(scope),
            TreeAction::Compare(schema) => self.open_compare(Some(schema)),
        }
    }
}

/// What a click in the table tree asks for, carried out once it is drawn.
enum TreeAction {
    Disconnect,
    Activity,
    /// In this schema, or the default one.
    NewTable(Option<String>),
    Diagram(String),
    ToggleSort,
    NewSql,
    Open(String, String),
    /// Star the table (or take the star off).
    ToggleStar(String, String),
    Structure(String, String),
    Sql(String),
    Template(String, String, Template),
    Object(DbObject),
    Run(DbObject),
    TableAction(TableActionPrompt),
    Dump(DumpScope),
    Compare(String),
}

/// "TABLES" with quiet icon actions, like a file tree.
fn tree_toolbar(ui: &mut egui::Ui, s: &Session, by_size: bool) -> Option<TreeAction> {
    let tk = theme::tokens();
    let mut action = None;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("TABLES").size(11.0).color(tk.subtle));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let button = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
                ui.add(egui::Button::new(theme::icon(glyph).color(tk.muted_foreground)).frame(false))
                    .on_hover_text(tip)
                    .clicked()
            };
            if button(ui, icon::PLUGS, "Disconnect") {
                action = Some(TreeAction::Disconnect);
            }
            if button(ui, icon::PULSE, "Server activity") {
                action = Some(TreeAction::Activity);
            }
            if !s.cfg.read_only && button(ui, icon::PLUS, "New table") {
                action = Some(TreeAction::NewTable(None));
            }
            let sort =
                theme::icon(icon::SORT_DESCENDING).color(if by_size { tk.lime } else { tk.muted_foreground });
            if ui
                .add(egui::Button::new(sort).frame(false))
                .on_hover_text(if by_size { "Sort by name" } else { "Sort by size" })
                .clicked()
            {
                action = Some(TreeAction::ToggleSort);
            }
            if button(ui, icon::ARROWS_CLOCKWISE, "Reload tables") {
                s.reload_tables();
            }
            if button(ui, icon::CODE, "New SQL editor") {
                action = Some(TreeAction::NewSql);
            }
        });
    });
    action
}

/// What the tree draws from, the same for every schema.
struct Tree<'a> {
    s: &'a Session,
    /// The filter, lowercased.
    needle: String,
    by_size: bool,
    /// The table of the active tab, highlighted.
    active_table: Option<(&'a str, &'a str)>,
    /// Starred tables of this connection, in the order they were starred.
    starred: &'a [(String, String)],
}

impl Tree<'_> {
    /// Where unqualified names go.
    fn default_schema(&self) -> &str {
        match self.s.cfg.kind {
            Kind::Postgres => &self.s.current_schema,
            Kind::MySql => &self.s.current_db,
            Kind::Sqlite => "main",
        }
    }

    /// The starred tables, above the schemas. Tables that no longer exist are skipped.
    fn starred_section(&self, ui: &mut egui::Ui) -> Option<TreeAction> {
        let tk = theme::tokens();
        let tables: Vec<&TableRef> = self
            .starred
            .iter()
            .filter_map(|(schema, name)| {
                self.s
                    .tables
                    .iter()
                    .find(|t| t.schema == *schema && t.name == *name)
            })
            .filter(|t| self.needle.is_empty() || t.search_key.contains(&self.needle))
            .collect();
        if tables.is_empty() {
            return None;
        }
        let header = egui::RichText::new(format!("★ Starred  {}", tables.len()))
            .size(11.0)
            .color(tk.warning);
        let mut action = None;
        egui::CollapsingHeader::new(header)
            .id_salt("starred")
            .default_open(true)
            .show(ui, |ui| {
                for t in tables {
                    ui.push_id(("starred", &t.schema, &t.name), |ui| {
                        if let Some(a) = self.table_row(ui, t) {
                            action = Some(a);
                        }
                    });
                }
            });
        action
    }

    /// One schema: its tables, then its other objects. `focus` opens it (and folds the others).
    fn schema_section(
        &self,
        ui: &mut egui::Ui,
        group: &[TableRef],
        focus: Option<&str>,
    ) -> Option<TreeAction> {
        let tk = theme::tokens();
        let mut visible: Vec<&TableRef> = group
            .iter()
            .filter(|t| self.needle.is_empty() || t.search_key.contains(&self.needle))
            .collect();
        if self.by_size {
            visible.sort_by_key(|t| std::cmp::Reverse(t.size.unwrap_or(-1)));
        }
        if visible.is_empty() {
            return None;
        }
        let schema = &group[0].schema;
        let is_default = schema == self.default_schema();
        let header = if is_default {
            egui::RichText::new(format!("{schema}  {}  · default", visible.len()))
                .size(11.0)
                .color(tk.foreground)
        } else {
            egui::RichText::new(format!("{schema}  {}", visible.len()))
                .size(11.0)
                .color(tk.subtle)
        };
        let mut action = None;
        let section = egui::CollapsingHeader::new(header)
            .id_salt(("schema", schema))
            .default_open(!self.needle.is_empty() || is_default || group.len() < AUTO_EXPAND_BELOW)
            .open(focus.map(|f| f == schema))
            .show(ui, |ui| {
                for t in visible {
                    if let Some(a) = self.table_row(ui, t) {
                        action = Some(a);
                    }
                }
                if let Some(a) = self.object_lists(ui, schema) {
                    action = Some(a);
                }
            });
        let header = if is_default {
            section
                .header_response
                .on_hover_text("Names without a schema in the editor (SELECT * FROM table) go here")
        } else {
            section.header_response
        };
        header.context_menu(|ui| {
            if ui.button("ER diagram of this schema").clicked() {
                action = Some(TreeAction::Diagram(schema.clone()));
                ui.close();
            }
            if !self.s.cfg.read_only && ui.button("New table here").clicked() {
                action = Some(TreeAction::NewTable(Some(schema.clone())));
                ui.close();
            }
            if ui.button("Export schema as .sql…").clicked() {
                action = Some(TreeAction::Dump(DumpScope::Schema(schema.clone())));
                ui.close();
            }
            if ui.button("Compare structure with…").clicked() {
                action = Some(TreeAction::Compare(schema.clone()));
                ui.close();
            }
        });
        action
    }

    /// A table or view: one click opens it; the menu has the rest.
    fn table_row(&self, ui: &mut egui::Ui, t: &TableRef) -> Option<TreeAction> {
        let tk = theme::tokens();
        let glyph = if t.is_view { icon::EYE } else { icon::TABLE };
        let is_open = self.active_table == Some((t.schema.as_str(), t.name.as_str()));
        let text_color = if is_open {
            tk.foreground
        } else {
            tk.muted_foreground
        };
        let r = ui.add(
            egui::Button::selectable(is_open, theme::icon_text(glyph, &t.name, tk.subtle, text_color))
                .frame_when_inactive(false)
                .min_size(egui::vec2(ui.available_width(), 22.0)),
        );
        let detail = if self.by_size {
            t.size.map(palette::format_bytes)
        } else {
            t.estimate.map(palette::compact_count)
        };
        let r = match (t.estimate, t.size) {
            (Some(n), Some(b)) => r.on_hover_text(format!(
                "~{} rows · {}",
                palette::format_count(n),
                palette::format_bytes(b)
            )),
            _ => r,
        };
        if let Some(count) = detail {
            let fits = (t.name.chars().count() + count.len()) as f32 * 7.0 + 40.0 < r.rect.width();
            if fits {
                ui.painter().text(
                    r.rect.right_center() - egui::vec2(6.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    count,
                    egui::FontId::proportional(11.0),
                    tk.subtle,
                );
            }
        }
        let mut action = r
            .clicked()
            .then(|| TreeAction::Open(t.schema.clone(), t.name.clone()));
        r.context_menu(|ui| {
            if let Some(a) = self.table_menu(ui, t) {
                action = Some(a);
                ui.close();
            }
        });
        action
    }

    fn table_menu(&self, ui: &mut egui::Ui, t: &TableRef) -> Option<TreeAction> {
        let s = self.s;
        let (schema, name) = (t.schema.clone(), t.name.clone());
        if ui.button("Open data").clicked() {
            return Some(TreeAction::Open(schema, name));
        }
        if ui.button("Structure").clicked() {
            return Some(TreeAction::Structure(schema, name));
        }
        let starred = crate::starred::is_starred(self.starred, &t.schema, &t.name);
        if ui
            .button(if starred {
                "Remove from starred"
            } else {
                "Star this table"
            })
            .clicked()
        {
            return Some(TreeAction::ToggleStar(schema, name));
        }
        if ui.button("SELECT in new editor").clicked() {
            let from = s.cfg.kind.qualified(&t.schema, &t.name);
            return Some(TreeAction::Sql(format!("SELECT * FROM {from} LIMIT 100;")));
        }
        let mut action = None;
        ui.menu_button("Generate SQL", |ui| {
            for (what, label) in [
                (Template::Select, "SELECT with the columns"),
                (Template::Insert, "INSERT"),
                (Template::Update, "UPDATE by key"),
                (Template::Delete, "DELETE by key"),
            ] {
                if ui.button(label).clicked() {
                    action = Some(TreeAction::Template(schema.clone(), name.clone(), what));
                    ui.close();
                }
            }
        });
        if action.is_some() {
            return action;
        }
        if !t.is_view && ui.button("Export as .sql…").clicked() {
            return Some(TreeAction::Dump(DumpScope::Table(schema, name)));
        }
        if !t.is_view && !s.cfg.read_only {
            ui.separator();
            for (what, label) in [
                (TableAction::Rename, "Rename…"),
                (TableAction::Duplicate(false), "Duplicate…"),
                (TableAction::Truncate, "Truncate (TRUNCATE)…"),
                (TableAction::Drop, "Drop table…"),
            ] {
                if ui.button(label).clicked() {
                    let input = match what {
                        TableAction::Duplicate(_) => format!("{}_copy", t.name),
                        TableAction::Rename => t.name.clone(),
                        _ => String::new(),
                    };
                    return Some(TreeAction::TableAction(TableActionPrompt {
                        schema,
                        table: name,
                        action: what,
                        input,
                    }));
                }
            }
        }
        if t.is_view && ui.button("View the view's code").clicked() {
            return s
                .objects
                .iter()
                .find(|o| o.kind == ObjKind::View && o.schema == t.schema && o.name == t.name)
                .cloned()
                .map(TreeAction::Object);
        }
        None
    }

    /// Functions, procedures, triggers and sequences of this schema (views are listed with
    /// the tables).
    fn object_lists(&self, ui: &mut egui::Ui, schema: &str) -> Option<TreeAction> {
        let tk = theme::tokens();
        let mut action = None;
        for kind in [
            ObjKind::Function,
            ObjKind::Procedure,
            ObjKind::Trigger,
            ObjKind::Sequence,
        ] {
            let objs: Vec<&DbObject> = self
                .s
                .objects
                .iter()
                .filter(|o| o.schema == schema && o.kind == kind)
                .filter(|o| self.needle.is_empty() || o.name.to_lowercase().contains(&self.needle))
                .collect();
            if objs.is_empty() {
                continue;
            }
            let header = egui::RichText::new(format!("{}  {}", kind.plural(), objs.len()))
                .size(11.0)
                .color(tk.subtle);
            egui::CollapsingHeader::new(header)
                .id_salt(("objs", schema, kind))
                .default_open(!self.needle.is_empty())
                .show(ui, |ui| {
                    for o in objs {
                        if let Some(a) = object_row(ui, o) {
                            action = Some(a);
                        }
                    }
                });
        }
        action
    }
}

/// A function, procedure, trigger or sequence: one click shows its code.
fn object_row(ui: &mut egui::Ui, o: &DbObject) -> Option<TreeAction> {
    let tk = theme::tokens();
    let glyph = match o.kind {
        ObjKind::Function => icon::FUNCTION,
        ObjKind::Procedure => icon::GEAR_SIX,
        ObjKind::Trigger => icon::LIGHTNING,
        ObjKind::Sequence => icon::LIST_NUMBERS,
        ObjKind::View => icon::EYE,
    };
    let r = ui.add(
        egui::Button::new(theme::icon_text(glyph, &o.name, tk.subtle, tk.muted_foreground))
            .frame_when_inactive(false)
            .truncate()
            .min_size(egui::vec2(ui.available_width(), 22.0)),
    );
    let tip = match o.kind {
        ObjKind::Trigger => format!("{} · table {}", o.name, o.table),
        ObjKind::Function | ObjKind::Procedure => format!("{}({})", o.name, o.args),
        _ => o.name.clone(),
    };
    let r = r.on_hover_text(format!("{tip}\nclick opens the code"));
    let mut action = r.clicked().then(|| TreeAction::Object(o.clone()));
    r.context_menu(|ui| {
        if matches!(o.kind, ObjKind::Function | ObjKind::Procedure)
            && ui
                .button("Run…")
                .on_hover_text("Asks for the arguments and shows the result")
                .clicked()
        {
            action = Some(TreeAction::Run(o.clone()));
            ui.close();
        }
        if ui.button("View code").clicked() {
            action = Some(TreeAction::Object(o.clone()));
            ui.close();
        }
        if ui.button("Copy name").clicked() {
            ui.ctx().copy_text(o.name.clone());
            ui.close();
        }
    });
    action
}

/// Connection name followed by a small engine tag in its color.
fn conn_label(c: &ConnConfig) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let t = theme::tokens();
    let (tag, color) = match c.kind {
        Kind::Postgres => ("PG", t.info),
        Kind::MySql => ("MySQL", t.warning),
        Kind::Sqlite => ("SQLite", t.boolean),
    };
    let mut job = LayoutJob::default();
    if let Some(tag) = c.color {
        let dot = TextFormat::simple(egui::FontId::proportional(11.0), theme::tag_color(tag));
        job.append("● ", 0.0, dot);
    }
    job.append(
        &c.name,
        0.0,
        TextFormat::simple(egui::FontId::proportional(13.0), t.foreground),
    );
    job.append(
        tag,
        8.0,
        TextFormat::simple(egui::FontId::proportional(10.0), color),
    );
    if c.read_only {
        let fmt = TextFormat::simple(egui::FontId::proportional(10.0), t.destructive);
        job.append("read-only", 6.0, fmt);
    } else if c.manual_commit {
        let fmt = TextFormat::simple(egui::FontId::proportional(10.0), t.warning);
        job.append("commit manual", 6.0, fmt);
    }
    job
}
