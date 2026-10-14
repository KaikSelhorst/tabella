//! The tab strip, and the body of each tab drawn by its own module (`editor`, `table`,
//! `results` for the grid both share, `activity`, `search`, `script`…). Drawing only: edits,
//! queries and deletions are carried out by `crate::app`.

use std::collections::HashSet;

use eframe::egui::{self, Align, Key, Layout, TextEdit};

use super::design::{self, DesignAction};
use super::detail::{self, DetailAction};
use super::diagram::{self, DiagramAction};
use super::grid::{self, GridOpts, grid};
use super::import_form::{self, ImportAction};
use super::structure::{self, InsertAction, StructAction};
use super::{chart, complete, find, icon, palette, plan_view, syntax, theme};
use crate::app::{self, App, Effect, FavPrompt, GridEvent, RunMode, Session, Tab, TabKind};
use crate::db::{ActivityView, QueryResult};
use crate::export::{self, Format};
use crate::favorites::{self, Favorite};
use crate::filters::{self, SavedFilter};
use crate::history::{self, Entry};
use crate::sql::{self, page_size};
use crate::values;

mod activity;
mod editor;
mod query_history;
mod results;
mod script;
mod search;
mod table;

use activity::*;
use editor::*;
use query_history::*;
use results::*;
use script::*;
use search::*;
use table::*;

/// What a tab body needs from the app besides the tab itself.
pub(super) struct Cx<'a> {
    s: &'a Session,
    show_detail: &'a mut bool,
    history: &'a [Entry],
    favorites: &'a [Favorite],
    filters: &'a [SavedFilter],
    /// Something only the app can do (open another tab), carried out after drawing.
    request: Option<Request>,
}

/// From the tab strip's context menu.
pub(super) enum TabAction {
    Rename(usize, String),
    Duplicate(usize),
    /// Close the others (`true`: only those to the right).
    CloseOthers(usize, bool),
    Reopen,
}

pub(super) enum Request {
    Structure(String, String),
    Data(String, String),
    Sql(String),
    OpenFiltered(String, String, String),
    SaveFavorite(String),
    DeleteFavorite(String),
    /// The ER diagram of a schema.
    Diagram(String),
    /// Save the active editor tab to its file (`true`: ask where).
    SaveFile(bool),
    OpenFile,
    RunFile,
    History,
    /// Remove one history entry: (connection, sql).
    ForgetHistory(String, String),
    /// "Editar estrutura" of schema.table (its Estrutura tab has the data).
    Design(String, String),
    /// schema, table, name, filter
    SaveFilter(String, String, String, String),
    DeleteFilter(String),
}

impl App {
    pub fn central(&mut self, ui: &mut egui::Ui) {
        if self.tabs.is_empty() || !self.session.as_ref().is_some_and(|s| s.ready) {
            self.welcome(ui);
            return;
        }
        let t = theme::tokens();
        let mut close = None;
        let mut action: Option<TabAction> = None;
        let strip = egui::Frame::new()
            .fill(t.sidebar)
            .inner_margin(egui::Margin::symmetric(8, 0))
            .show(ui, |ui| {
                ui.set_height(32.0);
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let glyph = match tab.kind {
                            TabKind::Table(_) => icon::TABLE,
                            TabKind::Sql(_) => icon::CODE,
                            TabKind::Structure(_) => icon::TREE_STRUCTURE,
                            TabKind::Activity(_) => icon::PULSE,
                            TabKind::Design(_) => icon::PENCIL_SIMPLE,
                            TabKind::Search(_) => icon::MAGNIFYING_GLASS,
                            TabKind::History(_) => icon::CLOCK_COUNTER_CLOCKWISE,
                            TabKind::Diagram(_) => icon::GRAPH,
                            TabKind::Script(_) => icon::PLAY,
                        };
                        let shown = if app::file_dirty(tab) {
                            format!("{} •", tab.title)
                        } else {
                            tab.title.clone()
                        };
                        let label = theme::tab_label(ui, glyph, &shown, i == self.active);
                        if label.clicked() {
                            self.active = i;
                        }
                        let is_sql = matches!(tab.kind, TabKind::Sql(_));
                        let closed = !self.closed.is_empty();
                        let n = self.tabs.len();
                        label.context_menu(|ui| {
                            // Rename in place: type and press Enter.
                            let name_id = egui::Id::new(("tab_name", tab.id));
                            let mut name = ui
                                .data(|d| d.get_temp::<String>(name_id))
                                .unwrap_or_else(|| tab.title.clone());
                            ui.label(egui::RichText::new("Nome da aba").size(11.0).color(t.subtle));
                            let r = ui.add(TextEdit::singleline(&mut name).desired_width(200.0));
                            if r.lost_focus()
                                && ui.input(|i| i.key_pressed(Key::Enter))
                                && !name.trim().is_empty()
                            {
                                action = Some(TabAction::Rename(i, name.trim().to_string()));
                                ui.data_mut(|d| d.remove::<String>(name_id));
                                ui.close();
                            } else {
                                ui.data_mut(|d| d.insert_temp(name_id, name));
                            }
                            ui.separator();
                            if is_sql && ui.button("Duplicar").clicked() {
                                action = Some(TabAction::Duplicate(i));
                                ui.close();
                            }
                            if ui
                                .add_enabled(n > 1, egui::Button::new("Fechar as outras"))
                                .clicked()
                            {
                                action = Some(TabAction::CloseOthers(i, false));
                                ui.close();
                            }
                            if ui
                                .add_enabled(i + 1 < n, egui::Button::new("Fechar as da direita"))
                                .clicked()
                            {
                                action = Some(TabAction::CloseOthers(i, true));
                                ui.close();
                            }
                            if ui
                                .add_enabled(
                                    closed,
                                    egui::Button::new("Reabrir aba fechada").shortcut_text("Ctrl+Shift+T"),
                                )
                                .clicked()
                            {
                                action = Some(TabAction::Reopen);
                                ui.close();
                            }
                        });
                        let x = theme::icon(icon::X).size(11.0).color(t.subtle);
                        if ui
                            .add(egui::Button::new(x).frame(false))
                            .on_hover_text("Fechar")
                            .clicked()
                        {
                            close = Some(i);
                        }
                        ui.add_space(12.0);
                    }
                });
            });
        let r = strip.response.rect;
        ui.painter()
            .hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, t.border));
        if let Some(i) = close {
            self.close_tab(i);
            return;
        }
        match action {
            Some(TabAction::Rename(i, name)) => {
                if let Some(tab) = self.tabs.get_mut(i) {
                    tab.title = name;
                    tab.renamed = true;
                }
            }
            Some(TabAction::Duplicate(i)) => self.duplicate_tab(i),
            Some(TabAction::CloseOthers(i, right)) => self.close_others(i, right),
            Some(TabAction::Reopen) => self.reopen_closed_tab(),
            None => {}
        }
        let (Some(s), Some(tab)) = (&self.session, self.tabs.get_mut(self.active)) else {
            return;
        };
        let mut cx = Cx {
            s,
            show_detail: &mut self.show_detail,
            history: &self.history,
            favorites: &self.favorites,
            filters: &self.filters,
            request: None,
        };
        match &tab.kind {
            TabKind::Sql(_) => sql_tab(ui, tab, &mut cx),
            TabKind::Table(_) => table_tab(ui, tab, &mut cx),
            TabKind::Activity(_) => activity_tab(ui, tab, &mut cx),
            TabKind::Search(_) => search_tab(ui, tab, &mut cx),
            TabKind::History(_) => history_tab(ui, tab, &mut cx),
            TabKind::Script(_) => script_tab(ui, tab, &mut cx),
            TabKind::Diagram(_) => {
                let busy = tab.busy();
                let schemas: Vec<String> = {
                    let mut v: Vec<String> = s.tables.iter().map(|t| t.schema.clone()).collect();
                    v.dedup();
                    v
                };
                let TabKind::Diagram(d) = &mut tab.kind else {
                    unreachable!()
                };
                match diagram::diagram_tab(ui, tab.id, d, busy, &schemas, s) {
                    Some(DiagramAction::OpenTable(table)) => {
                        cx.request = Some(Request::Data(d.schema.clone(), table))
                    }
                    Some(DiagramAction::Schema(schema)) => cx.request = Some(Request::Diagram(schema)),
                    None => {}
                }
            }
            TabKind::Design(_) => {
                let busy = tab.busy();
                if let Some(e) = &tab.error {
                    error_banner(ui, e);
                }
                let TabKind::Design(d) = &mut tab.kind else {
                    unreachable!()
                };
                match design::design_tab(ui, tab.id, d, busy) {
                    Some(DesignAction::Run(sql)) => {
                        if s.cfg.read_only {
                            tab.error =
                                Some("Conexão somente leitura: não é possível alterar a estrutura.".into());
                        } else if sql::script_risk(&sql).dangers.is_empty() {
                            app::run_sql(tab, s, sql);
                        } else {
                            // ALTER / DROP: same confirmation as in the editor.
                            tab.confirm_run = Some(sql);
                        }
                    }
                    Some(DesignAction::OpenSql(sql)) => cx.request = Some(Request::Sql(sql)),
                    None => {}
                }
                confirm_run(ui.ctx(), tab, s);
            }
            TabKind::Structure(st) => {
                if let Some(e) = &tab.error {
                    error_banner(ui, e);
                }
                cx.request = match structure::structure_tab(ui, st, tab.busy()) {
                    Some(StructAction::OpenSql(ddl)) => Some(Request::Sql(ddl)),
                    Some(StructAction::Edit) => Some(Request::Design(st.schema.clone(), st.table.clone())),
                    Some(StructAction::OpenData) => Some(Request::Data(st.schema.clone(), st.table.clone())),
                    None => None,
                };
            }
        }
        match cx.request {
            Some(Request::Structure(schema, table)) => self.open_structure(&schema, &table),
            Some(Request::Data(schema, table)) => self.open_table(&schema, &table),
            Some(Request::OpenFiltered(schema, table, filter)) => {
                self.open_table_filtered(&schema, &table, filter)
            }
            Some(Request::Sql(text)) => self.open_sql(text),
            Some(Request::SaveFavorite(sql)) => {
                self.fav_prompt = Some(FavPrompt {
                    name: String::new(),
                    sql,
                    shared: false,
                });
            }
            Some(Request::DeleteFavorite(id)) => self.delete_favorite(&id),
            Some(Request::Diagram(schema)) => self.open_diagram(Some(&schema)),
            Some(Request::SaveFile(ask)) => self.save_file(self.active, ask),
            Some(Request::OpenFile) => self.open_file_dialog(),
            Some(Request::RunFile) => self.pick_script(),
            Some(Request::History) => self.open_history(),
            Some(Request::ForgetHistory(conn, sql)) => self.forget_history(&conn, &sql),
            Some(Request::Design(schema, table)) => {
                let data = self.tabs.iter().find_map(|t| match &t.kind {
                    TabKind::Structure(st) if st.schema == schema && st.table == table => st.data.clone(),
                    _ => None,
                });
                if let Some(data) = data {
                    self.open_design(&schema, &table, &data);
                }
            }
            Some(Request::SaveFilter(schema, table, name, filter)) => {
                self.save_filter(&schema, &table, &name, &filter);
            }
            Some(Request::DeleteFilter(id)) => self.delete_filter(&id),
            None => {}
        }
    }
}

pub(super) fn error_banner(ui: &mut egui::Ui, e: &str) {
    let t = theme::tokens();
    egui::Frame::new()
        .fill(theme::mix(t.destructive, 0.12, t.background))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(egui::RichText::new(e).color(t.destructive)).wrap());
        });
}

/// "Exportar" dropdown: what is on screen, or (table tabs) the whole table. Returns the choice;
/// the caller starts the export once it can borrow the tab again.
pub(super) fn export_menu(ui: &mut egui::Ui, is_table: bool, enabled: bool) -> Option<(Format, bool)> {
    let t = theme::tokens();
    let label = theme::icon_text(
        icon::DOWNLOAD_SIMPLE,
        "Exportar",
        t.muted_foreground,
        t.foreground,
    );
    let mut chosen = None;
    ui.add_enabled_ui(enabled, |ui| {
        ui.menu_button(label, |ui| {
            ui.set_width(220.0);
            let heading = |ui: &mut egui::Ui, text: &str| {
                ui.label(egui::RichText::new(text).size(11.0).color(t.subtle));
            };
            heading(ui, if is_table { "PÁGINA ATUAL" } else { "RESULTADO" });
            for f in Format::ALL {
                if ui.button(f.label()).clicked() {
                    chosen = Some((f, false));
                    ui.close();
                }
            }
            if is_table {
                ui.separator();
                heading(ui, "TABELA INTEIRA (COM FILTRO)");
                for f in Format::ALL {
                    if ui.button(f.label()).clicked() {
                        chosen = Some((f, true));
                        ui.close();
                    }
                }
            }
            ui.separator();
            ui.label(
                egui::RichText::new("Salvo na pasta Downloads")
                    .size(11.0)
                    .color(t.subtle),
            );
        });
    });
    chosen
}

/// "Exportado N linhas para arquivo · Abrir pasta · ×" after an export finishes.
pub(super) fn export_banner(ui: &mut egui::Ui, tab: &mut Tab) {
    let Some((path, rows)) = &tab.exported else { return };
    let t = theme::tokens();
    let mut dismiss = false;
    egui::Frame::new()
        .fill(theme::mix(t.lime, 0.10, t.background))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::CHECK).color(t.lime));
                let name = path
                    .file_name()
                    .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().to_string());
                ui.label(
                    egui::RichText::new(format!(
                        "{} linha(s) exportada(s) para {name}",
                        palette::format_count(*rows as i64)
                    ))
                    .color(t.foreground),
                )
                .on_hover_text(path.display().to_string());
                if ui.link("Abrir pasta").clicked() {
                    export::reveal(path);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    dismiss = ui
                        .add(egui::Button::new(theme::icon(icon::X).size(11.0).color(t.subtle)).frame(false))
                        .clicked();
                });
            });
        });
    if dismiss {
        tab.exported = None;
    }
}

/// Spinner plus a button that stops the running statement on the server.
pub(super) fn busy_indicator(ui: &mut egui::Ui, s: &Session) {
    let t = theme::tokens();
    ui.spinner();
    let stop = egui::Button::new(theme::icon_text(
        icon::STOP,
        "Cancelar",
        t.destructive,
        t.foreground,
    ));
    if ui
        .add(stop)
        .on_hover_text("Interrompe a consulta no servidor")
        .clicked()
    {
        s.cancel();
    }
}

/// Red button for the destructive choice in a dialog. Never the keyboard default: Enter does
/// nothing in those dialogs, only a click confirms.
pub(super) fn danger_button(label: &str) -> egui::Button<'static> {
    let t = theme::tokens();
    egui::Button::new(egui::RichText::new(label.to_string()).color(t.foreground)).fill(t.destructive)
}

/// Icon toggle for the row detail panel.
pub(super) fn detail_toggle(ui: &mut egui::Ui, show: &mut bool) {
    let t = theme::tokens();
    let color = if *show { t.lime } else { t.muted_foreground };
    let tip = if *show {
        "Esconder detalhes da linha"
    } else {
        "Mostrar detalhes da linha"
    };
    if ui
        .button(theme::icon(icon::SIDEBAR_SIMPLE).color(color))
        .on_hover_text(tip)
        .clicked()
    {
        *show = !*show;
    }
}
