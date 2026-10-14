//! The tab strip and the body of each tab (SQL editor or table browser). Drawing only: edits,
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

/// What a tab body needs from the app besides the tab itself.
struct Cx<'a> {
    s: &'a Session,
    show_detail: &'a mut bool,
    history: &'a [Entry],
    favorites: &'a [Favorite],
    filters: &'a [SavedFilter],
    /// Something only the app can do (open another tab), carried out after drawing.
    request: Option<Request>,
}

/// From the tab strip's context menu.
enum TabAction {
    Rename(usize, String),
    Duplicate(usize),
    /// Close the others (`true`: only those to the right).
    CloseOthers(usize, bool),
    Reopen,
}

enum Request {
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
                            TabKind::Sql { .. } => icon::CODE,
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
                        let is_sql = matches!(tab.kind, TabKind::Sql { .. });
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
            TabKind::Sql { .. } => sql_tab(ui, tab, &mut cx),
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

fn error_banner(ui: &mut egui::Ui, e: &str) {
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
fn export_menu(ui: &mut egui::Ui, is_table: bool, enabled: bool) -> Option<(Format, bool)> {
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
fn export_banner(ui: &mut egui::Ui, tab: &mut Tab) {
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
fn busy_indicator(ui: &mut egui::Ui, s: &Session) {
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

fn sql_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let busy = tab.busy();
    let can_export = !busy && tab.result.as_ref().is_some_and(|r| !r.columns.is_empty());
    let mut export = None;
    let tab_last_query = tab.last_query.clone();
    let mut repeat_value = tab.repeat;
    let repeat = &mut repeat_value;
    let TabKind::Sql { text } = &mut tab.kind else {
        return;
    };
    let mut run: Option<String> = None;
    let mut explain: Option<(String, bool)> = None;
    let mut preview: Option<String> = None;
    let mut open_table: Option<(String, String)> = None;
    let mut file_menu_action: Option<Request> = None;
    let has_file = tab.file.is_some();
    let t = theme::tokens();
    let f5 = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F5));
    egui::Panel::top(egui::Id::new(("editor", tab.id)))
        .frame(
            egui::Frame::new()
                .fill(t.background)
                .inner_margin(egui::Margin::same(8)),
        )
        .resizable(true)
        .default_size(200.0)
        .show(ui, |ui| {
            let editor_id = egui::Id::new(("sql", tab.id));
            // Selection survives losing focus (e.g. clicking Executar), so read it from the stored state.
            let cursor = TextEdit::load_state(ui.ctx(), editor_id).and_then(|st| st.cursor.char_range());
            let selected_range = cursor
                .map(|r| r.as_sorted_char_range())
                .map(|r| (usize::from(r.start), usize::from(r.end)))
                .filter(|(a, b)| a < b);
            let chars = |(a, b): (usize, usize)| text.chars().skip(a).take(b - a).collect::<String>();
            let selection = selected_range.map(chars);
            // With several statements, Ctrl+Enter runs the one under the cursor and F5 runs all.
            let several = sql::statement_ranges(text, true).len() > 1;
            let current = if several && selection.is_none() {
                cursor.and_then(|c| sql::statement_at(text, usize::from(c.primary.index)))
            } else {
                None
            };
            let statement = current.map(chars);
            // What Ctrl+Enter / the main button runs, and what F5 / "Executar tudo" runs.
            let one = selection
                .clone()
                .or_else(|| statement.clone())
                .unwrap_or_else(|| text.clone());
            let all = selection.clone().unwrap_or_else(|| text.clone());
            let format_key =
                ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT | egui::Modifiers::ALT, Key::F));
            ui.horizontal(|ui| {
                let label = if selection.is_some() {
                    "Executar seleção"
                } else if statement.is_some() {
                    "Executar comando"
                } else {
                    "Executar"
                };
                if ui
                    .add_enabled(!busy, theme::primary_button(Some(icon::PLAY), label))
                    .on_hover_text(if statement.is_some() {
                        "Ctrl+Enter · o comando sob o cursor"
                    } else {
                        "Ctrl+Enter ou F5"
                    })
                    .clicked()
                {
                    run = Some(one.clone());
                }
                if statement.is_some()
                    && ui
                        .add_enabled(!busy, theme::outline_button("Executar tudo"))
                        .on_hover_text("F5 · todos os comandos, um resultado para cada consulta")
                        .clicked()
                {
                    run = Some(all.clone());
                }
                if app::previewable(&one)
                    && ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(theme::icon_text(icon::EYE, "Prévia", t.muted_foreground, t.foreground)),
                        )
                        .on_hover_text("Ctrl+Shift+Enter · quantas e quais linhas este UPDATE/DELETE alteraria, sem alterar")
                        .clicked()
                {
                    preview = Some(one.clone());
                }
                if busy {
                    busy_indicator(ui, s);
                }
                match history_menu(ui, cx.history, &s.cfg.name) {
                    Some(HistoryPick::Load(sql)) => *text = sql,
                    Some(HistoryPick::All) => file_menu_action = Some(Request::History),
                    None => {}
                }
                match favorites_menu(ui, cx.favorites, &s.cfg.id, !text.trim().is_empty()) {
                    Some(FavAction::Load(sql)) => *text = sql,
                    Some(FavAction::Save) => {
                        let sql = selection.clone().unwrap_or_else(|| text.clone());
                        cx.request = Some(Request::SaveFavorite(sql));
                    }
                    Some(FavAction::Delete(id)) => cx.request = Some(Request::DeleteFavorite(id)),
                    None => {}
                }
                export = export_menu(ui, false, can_export);
                let explain_label =
                    theme::icon_text(icon::TREE_VIEW, "Explicar", t.muted_foreground, t.foreground);
                ui.menu_button(explain_label, |ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Plano estimado").shortcut_text("Ctrl+E"))
                        .on_hover_text("EXPLAIN: como o banco pretende executar, sem executar")
                        .clicked()
                    {
                        explain = Some((one.clone(), false));
                        ui.close();
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Plano executado (ANALYZE)"))
                        .on_hover_text("EXPLAIN ANALYZE: executa a consulta e mede cada etapa (só leitura)")
                        .clicked()
                    {
                        explain = Some((one.clone(), true));
                        ui.close();
                    }
                });
                // "Repetir a cada": the last query again on a timer (reads only).
                let last_reads = !tab_last_query.trim().is_empty() && !sql::script_risk(&tab_last_query).writes;
                let repeat_label = match *repeat {
                    Some(n) => theme::icon_text(icon::ARROWS_CLOCKWISE, &format!("a cada {n} s"), t.lime, t.lime),
                    None => theme::icon_text(icon::ARROWS_CLOCKWISE, "Repetir", t.muted_foreground, t.foreground),
                };
                ui.add_enabled_ui(last_reads || repeat.is_some(), |ui| {
                    ui.menu_button(repeat_label, |ui| {
                        ui.label(
                            egui::RichText::new("Roda a última consulta de novo, sozinha")
                                .size(11.0)
                                .color(t.subtle),
                        );
                        for (n, label) in [(None, "Desligado"), (Some(2), "2 s"), (Some(5), "5 s"), (Some(10), "10 s"), (Some(30), "30 s"), (Some(60), "1 min")] {
                            if ui.selectable_label(*repeat == n, label).clicked() {
                                *repeat = n;
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_disabled_hover_text("Execute uma consulta de leitura primeiro (SELECT…)");
                });
                let file_label = theme::icon(icon::FOLDER_OPEN).color(t.muted_foreground);
                ui.menu_button(file_label, |ui| {
                    if ui.add(egui::Button::new("Abrir arquivo…").shortcut_text("Ctrl+O")).clicked() {
                        file_menu_action = Some(Request::OpenFile);
                        ui.close();
                    }
                    if !s.cfg.read_only
                        && ui
                            .button("Executar arquivo…")
                            .on_hover_text("Roda um .sql grande comando a comando, sem abrir no editor")
                            .clicked()
                    {
                        file_menu_action = Some(Request::RunFile);
                        ui.close();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("Salvar").shortcut_text("Ctrl+S")).clicked() {
                        file_menu_action = Some(Request::SaveFile(false));
                        ui.close();
                    }
                    if ui.add(egui::Button::new("Salvar como…").shortcut_text("Ctrl+Shift+S")).clicked() {
                        file_menu_action = Some(Request::SaveFile(true));
                        ui.close();
                    }
                })
                .response
                .on_hover_text(if has_file { "Arquivo .sql desta aba" } else { "Abrir ou salvar como arquivo .sql" });
                let format = theme::icon_text(
                    icon::TEXT_ALIGN_LEFT,
                    "Formatar",
                    t.muted_foreground,
                    t.foreground,
                );
                if ui
                    .button(format)
                    .on_hover_text("Shift+Alt+F · formata a seleção, se houver")
                    .clicked()
                    || format_key
                {
                    format_in_place(text, selected_range, s.cfg.kind);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    detail_toggle(ui, cx.show_detail);
                    // Where unqualified names go, so an UPDATE doesn't land somewhere unexpected.
                    let here = match s.cfg.kind {
                        crate::db::Kind::Postgres if !s.current_schema.is_empty() => {
                            format!("em {} · {}", s.current_db, s.current_schema)
                        }
                        _ if !s.current_db.is_empty() => format!("em {}", s.current_db),
                        _ => String::new(),
                    };
                    ui.label(egui::RichText::new(here).size(11.0).monospace().color(t.muted_foreground))
                        .on_hover_text("Banco e schema dos nomes sem prefixo (troque na sidebar)");
                    let keys = if several {
                        "Ctrl+Enter comando · F5 tudo"
                    } else {
                        "Ctrl+Enter / F5"
                    };
                    ui.label(egui::RichText::new(keys).size(11.0).color(t.subtle));
                });
            });
            ui.add_space(4.0);
            // Ctrl+F / Ctrl+H: find bar above the editor (state kept in egui memory per tab).
            let find_id = egui::Id::new(("find", tab.id));
            let mut find: Option<find::FindState> = ui.data(|d| d.get_temp(find_id));
            let editor_focused = ui.memory(|m| m.has_focus(editor_id));
            if editor_focused || find.is_some() {
                let (ctrl_f, ctrl_h) = ui.input_mut(|i| {
                    (
                        i.consume_key(egui::Modifiers::COMMAND, Key::F),
                        i.consume_key(egui::Modifiers::COMMAND, Key::H),
                    )
                });
                if ctrl_f || ctrl_h {
                    let st = find.get_or_insert_with(Default::default);
                    st.focus = true;
                    st.show_replace |= ctrl_h;
                    // Selected text (one line) becomes the search.
                    if let Some(sel) = selection.as_ref().filter(|s| !s.contains('\n')) {
                        st.query = sel.clone();
                    }
                }
            }
            let mut goto = false;
            let mut close_find = false;
            if let Some(st) = &mut find {
                let ranges = find::find_all(text, &st.query, st.case_sensitive);
                match find::find_bar(ui, st, ranges.len()) {
                    Some(find::FindAction::Goto) => goto = true,
                    Some(find::FindAction::ReplaceOne) => {
                        if let Some(r) = ranges.get(st.current.min(ranges.len().saturating_sub(1))) {
                            text.replace_range(r.clone(), &st.replace);
                            goto = true;
                        }
                    }
                    Some(find::FindAction::ReplaceAll) => {
                        find::replace_all(text, &st.query, &st.replace, st.case_sensitive);
                    }
                    Some(find::FindAction::Close) => close_find = true,
                    None => {}
                }
                ui.add_space(4.0);
            }
            if close_find {
                find = None;
                ui.memory_mut(|m| m.request_focus(editor_id));
            }
            let (query, case, current_match) = find
                .as_ref()
                .map(|f| (f.query.clone(), f.case_sensitive, f.current))
                .unwrap_or_default();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let catalog = complete::Catalog {
                        tables: &s.tables,
                        columns: &s.columns,
                        fks: &s.fks,
                    };
                    let popup = complete::before_editor(ui.ctx(), editor_id, text, catalog);
                    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                        let mut job = syntax::highlight(buf.as_str(), egui::FontId::monospace(13.0));
                        if !query.is_empty() {
                            let ranges = find::find_all(buf.as_str(), &query, case);
                            find::mark(&mut job, &ranges, current_match);
                        }
                        job.wrap.max_width = wrap;
                        ui.fonts_mut(|f| f.layout_job(job))
                    };
                    let mut out = TextEdit::multiline(text)
                        .id(editor_id)
                        .code_editor()
                        .hint_text("SELECT * FROM …")
                        .desired_width(f32::INFINITY)
                        .min_size(ui.available_size())
                        .lock_focus(true)
                        .layouter(&mut layouter)
                        .show(ui);
                    // Selects the current match (ready for typing over when the editor gets focus
                    // back) and scrolls to it.
                    if goto {
                        let ranges = find::find_all(text, &query, case);
                        if let Some(r) = ranges.get(current_match.min(ranges.len().saturating_sub(1))) {
                            let (a, b) = (text[..r.start].chars().count(), text[..r.end].chars().count());
                            let sel = egui::text::CCursorRange::two(
                                egui::text::CCursor::new(a),
                                egui::text::CCursor::new(b),
                            );
                            out.state.cursor.set_char_range(Some(sel));
                            out.state.clone().store(ui.ctx(), editor_id);
                            let rect = out.galley.pos_from_cursor(egui::text::CCursor::new(a));
                            ui.scroll_to_rect(rect.translate(out.galley_pos.to_vec2()), Some(Align::Center));
                        }
                    }
                    // Lime bar in the gutter beside the statement Ctrl+Enter would run.
                    if let Some((a, b)) = current {
                        let top = out.galley.pos_from_cursor(egui::text::CCursor::new(a));
                        let bottom = out.galley.pos_from_cursor(egui::text::CCursor::new(b));
                        let x = out.galley_pos.x - 5.0;
                        ui.painter().line_segment(
                            [
                                egui::pos2(x, out.galley_pos.y + top.min.y),
                                egui::pos2(x, out.galley_pos.y + bottom.max.y),
                            ],
                            egui::Stroke::new(2.0, t.lime),
                        );
                    }
                    // A table name under the pointer: its columns on hover, Ctrl+click opens it.
                    let hovered = out.response.hover_pos().and_then(|p| {
                        let at = out.galley.cursor_from_pos(p - out.galley_pos);
                        let (a, b) = sql::word_at(text, usize::from(at.index))?;
                        let word: String = text.chars().skip(a).take(b - a).collect();
                        table_named(s, &word)
                    });
                    if let Some(t) = hovered {
                        let ctrl = ui.input(|i| i.modifiers.command);
                        if ctrl {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if ctrl && out.response.clicked() {
                            open_table = Some((t.schema.clone(), t.name.clone()));
                        }
                        out.response.response.clone().on_hover_ui_at_pointer(|ui| table_tooltip(ui, s, t));
                    }
                    if let (Some((sugg, selected)), Some(range)) = (popup, out.cursor_range) {
                        let at = out.galley.pos_from_cursor(range.primary);
                        let anchor = out.galley_pos + at.left_bottom().to_vec2() + egui::vec2(0.0, 2.0);
                        complete::popup(ui.ctx(), editor_id, anchor, &sugg, selected);
                    }
                    let ctrl_enter = ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
                    let ctrl_e = out.response.has_focus()
                        && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::E));
                    if ctrl_e {
                        explain = Some((one.clone(), false));
                    }
                    let shift = ui.input(|i| i.modifiers.shift);
                    if out.response.has_focus() && ctrl_enter && shift {
                        if app::previewable(&one) {
                            preview = Some(one.clone());
                        }
                    } else if out.response.has_focus() && ctrl_enter {
                        run = Some(one.clone());
                    } else if f5 {
                        run = Some(all.clone());
                    }
                });
            ui.data_mut(|d| match find {
                Some(f) => {
                    d.insert_temp(find_id, f);
                }
                None => d.remove::<find::FindState>(find_id),
            });
        });
    if let Some((schema, table)) = open_table {
        cx.request = Some(Request::Data(schema, table));
    }
    tab.repeat = repeat_value;
    // "Repetir a cada": stops at the first error, so a broken query doesn't keep failing.
    if let Some(every) = tab.repeat {
        if tab.error.is_some() {
            tab.repeat = None;
        } else if !tab.busy() {
            let wait = std::time::Duration::from_secs(every);
            match tab.last_run.map(|at| at.elapsed()) {
                Some(passed) if passed < wait => ui.ctx().request_repaint_after(wait - passed),
                _ => app::rerun(tab, s),
            }
        }
    }
    if let Some(sql) = run.filter(|q| !q.trim().is_empty())
        && !busy
    {
        request_run(tab, s, sql, RunMode::Run);
    }
    if let Some((sql, analyze)) = explain.filter(|(q, _)| !q.trim().is_empty())
        && !busy
    {
        request_run(tab, s, sql, RunMode::Explain(analyze));
    }
    if let Some(sql) = preview
        && !busy
    {
        request_run(tab, s, sql, RunMode::Preview);
    }
    params_prompt(ui.ctx(), tab, s);
    preview_dialog(ui.ctx(), tab, s);
    if let Some((format, _)) = export {
        app::start_export(tab, s, format, false);
    }
    confirm_run(ui.ctx(), tab, s);
    export_banner(ui, tab);
    // Ctrl+S saves the result's pending edits when there are any, the file otherwise;
    // Ctrl+Shift+S always asks where to save the file (checked first: Ctrl+S would match it).
    let (save_as, save) = ui.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, Key::S),
            i.consume_key(egui::Modifiers::COMMAND, Key::S),
        )
    });
    let pending_edits = app::editable_ref(&tab.kind, &tab.source).is_some_and(|ts| !ts.changes.is_empty());
    if save && pending_edits {
        if let Some(ts) = app::editable_mut(&mut tab.kind, &mut tab.source) {
            ts.confirm_save = true;
        }
    } else if save || save_as {
        cx.request = Some(Request::SaveFile(save_as));
    }
    if file_menu_action.is_some() {
        cx.request = file_menu_action;
    }
    // Ctrl+Z undoes a result edit (in the editor it undoes typing).
    if !ui.ctx().egui_wants_keyboard_input()
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::Z))
        && let Some(ts) = app::editable_mut(&mut tab.kind, &mut tab.source)
    {
        ts.changes.pop();
    }
    changes_bar(ui, tab, s);
    confirm_save(ui.ctx(), tab, s);
    if let Some(plan) = &tab.plan {
        if let Some(e) = &tab.error {
            error_banner(ui, e);
        }
        let hints = index_suggestions(s, plan);
        match plan_view::plan_view(ui, plan, &hints) {
            Some(plan_view::PlanAction::Close) => tab.plan = None,
            Some(plan_view::PlanAction::OpenSql(sql)) => cx.request = Some(Request::Sql(sql)),
            None => {}
        }
        return;
    }
    result_area(ui, tab, cx, false);
}

/// The sidebar's table for a name typed in the editor (`clientes`, `public.clientes`,
/// `"Clientes"`), ignoring case when there's no exact match.
fn table_named<'a>(s: &'a Session, word: &str) -> Option<&'a crate::db::TableRef> {
    let parts: Vec<String> = word
        .split('.')
        .map(|p| p.trim_matches(|c| c == '"' || c == '`').to_string())
        .collect();
    let (schema, name) = match &parts[..] {
        [n] => (None, n.as_str()),
        [sc, n] => (Some(sc.as_str()), n.as_str()),
        _ => return None,
    };
    let find = |exact: bool| {
        let same = |a: &str, b: &str| if exact { a == b } else { a.eq_ignore_ascii_case(b) };
        s.tables
            .iter()
            .find(|t| same(&t.name, name) && schema.is_none_or(|sc| same(&t.schema, sc)))
    };
    find(true).or_else(|| find(false))
}

/// Hover card for a table name in the editor: its columns and types.
fn table_tooltip(ui: &mut egui::Ui, s: &Session, t: &crate::db::TableRef) {
    let tk = theme::tokens();
    ui.horizontal(|ui| {
        ui.label(theme::icon(if t.is_view { icon::EYE } else { icon::TABLE }).color(tk.muted_foreground));
        ui.label(egui::RichText::new(format!("{}.{}", t.schema, t.name)).color(tk.foreground));
        if let Some(n) = t.estimate {
            ui.label(
                egui::RichText::new(format!("~{} linhas", palette::format_count(n)))
                    .size(11.0)
                    .color(tk.subtle),
            );
        }
    });
    let cols: Vec<&crate::db::SchemaColumn> = s
        .schema_cols
        .iter()
        .filter(|c| c.schema == t.schema && c.table == t.name)
        .collect();
    egui::Grid::new(("table_tip", &t.schema, &t.name))
        .spacing([12.0, 2.0])
        .show(ui, |ui| {
            for c in cols.iter().take(TOOLTIP_COLUMNS) {
                ui.label(
                    egui::RichText::new(&c.column)
                        .monospace()
                        .size(12.0)
                        .color(tk.foreground),
                );
                ui.label(egui::RichText::new(&c.data_type).size(11.0).color(tk.number));
                ui.end_row();
            }
        });
    if cols.len() > TOOLTIP_COLUMNS {
        ui.label(
            egui::RichText::new(format!("… mais {}", cols.len() - TOOLTIP_COLUMNS))
                .size(11.0)
                .color(tk.subtle),
        );
    }
    ui.label(
        egui::RichText::new("Ctrl+clique abre a tabela")
            .size(11.0)
            .color(tk.subtle),
    );
}

/// Columns listed in the editor's hover card.
const TOOLTIP_COLUMNS: usize = 16;

/// Tables smaller than this aren't worth an index suggestion: reading them whole is cheap.
const INDEX_HINT_ROWS: i64 = 10_000;

/// `CREATE INDEX` suggestions for a plan: only for known tables of some size, and only with
/// columns that really are in them.
fn index_suggestions(s: &Session, plan: &crate::plan::Plan) -> Vec<(String, String)> {
    let k = s.cfg.kind;
    crate::plan::index_hints(plan)
        .into_iter()
        .filter_map(|h| {
            let t = s.tables.iter().find(|t| !t.is_view && t.name == h.table)?;
            if t.estimate.is_some_and(|n| n < INDEX_HINT_ROWS) {
                return None;
            }
            let cols: Vec<&String> = h
                .columns
                .iter()
                .filter(|c| s.columns.iter().any(|(tb, col)| *tb == t.name && col == *c))
                .collect();
            if cols.is_empty() {
                return None;
            }
            let names: Vec<&str> = cols.iter().map(|c| c.as_str()).collect();
            let sql = format!(
                "CREATE INDEX {} ON {} ({});",
                k.quote_ident(&format!("{}_{}_idx", t.name, names.join("_"))),
                k.qualified(&t.schema, &t.name),
                names
                    .iter()
                    .map(|c| k.quote_ident(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            Some((format!("{} ({})", t.name, names.join(", ")), sql))
        })
        .collect()
}

/// Runs, explains or previews editor SQL, asking for `:parameters` first.
fn request_run(tab: &mut Tab, s: &Session, sql: String, mode: RunMode) {
    let names = sql::param_names(&sql);
    if names.is_empty() {
        run_now(tab, s, sql, mode);
    } else {
        tab.params = Some(app::ParamPrompt {
            sql,
            names,
            mode,
            focused: false,
        });
    }
}

fn run_now(tab: &mut Tab, s: &Session, sql: String, mode: RunMode) {
    match mode {
        RunMode::Explain(analyze) => return app::explain(tab, s, &sql, analyze),
        RunMode::Preview => return app::preview(tab, s, sql),
        // Connections set to "prévia antes de alterar" look first.
        RunMode::Run if s.cfg.preview_writes && app::previewable(&sql) && !s.cfg.read_only => {
            return app::preview(tab, s, sql);
        }
        RunMode::Run | RunMode::RunPreviewed => {}
    }
    let risk = sql::script_risk(&sql);
    if s.cfg.read_only && risk.writes {
        tab.error =
            Some("Conexão somente leitura: só consultas (SELECT, SHOW, EXPLAIN…) podem rodar.".into());
    } else if !risk.dangers.is_empty() {
        tab.confirm_run = Some(sql);
    } else {
        app::run_sql(tab, s, sql);
    }
}

/// "Parâmetros": one field per `:name`, remembered per tab for the next run.
fn params_prompt(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let Some(p) = &mut tab.params else { return };
    let t = theme::tokens();
    let (mut go, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new(("params", tab.id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(420.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::BRACKETS_CURLY).size(16.0).color(t.lime));
                ui.label(egui::RichText::new("Parâmetros").size(15.0).color(t.foreground));
            });
            ui.label(
                egui::RichText::new("Números e NULL entram como estão; o resto vira texto entre aspas.")
                    .size(11.0)
                    .color(t.subtle),
            );
            ui.add_space(8.0);
            egui::Grid::new(("params_grid", tab.id))
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    for (i, name) in p.names.iter().enumerate() {
                        ui.label(
                            egui::RichText::new(format!(":{name}"))
                                .monospace()
                                .color(t.number),
                        );
                        let v = tab.param_values.entry(name.clone()).or_default();
                        let r = ui.add(
                            TextEdit::singleline(v)
                                .font(egui::FontId::monospace(12.5))
                                .desired_width(260.0),
                        );
                        if i == 0 && !p.focused {
                            r.request_focus();
                            p.focused = true;
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && i + 1 == p.names.len()
                        {
                            go = true;
                        }
                        ui.end_row();
                    }
                });
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = match p.mode {
                    RunMode::Explain(_) => "Explicar",
                    RunMode::Preview => "Prévia",
                    RunMode::Run | RunMode::RunPreviewed => "Executar",
                };
                if ui.add(theme::primary_button(Some(icon::PLAY), label)).clicked() {
                    go = true;
                }
                if ui.add(theme::outline_button("Cancelar")).clicked() {
                    cancel = true;
                }
            });
        });
    if go {
        let p = tab.params.take().expect("open prompt");
        let values = &tab.param_values;
        let sql = sql::bind_params(s.cfg.kind, &p.sql, |n| values.get(n).cloned().unwrap_or_default());
        run_now(tab, s, sql, p.mode);
    } else if cancel || modal.should_close() {
        tab.params = None;
    }
}

/// "Prévia": how many rows an UPDATE/DELETE would touch and some of them; run it from here or
/// give up.
fn preview_dialog(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let Some(p) = &tab.preview else { return };
    let t = theme::tokens();
    let delete = sql::statement_words_first(&p.sql).is_some_and(|w| w == "DELETE");
    let (mut run, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new(("preview", tab.id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(720.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::EYE).size(16.0).color(t.lime));
                ui.label(egui::RichText::new("Prévia").size(15.0).color(t.foreground));
                ui.label(
                    egui::RichText::new("nada foi alterado")
                        .size(11.0)
                        .color(t.subtle),
                );
            });
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(t.sidebar)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let job = syntax::highlight(&p.sql, egui::FontId::monospace(12.0));
                    ui.add(egui::Label::new(job).wrap());
                });
            ui.add_space(8.0);
            match &p.data {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Contando…").color(t.muted_foreground));
                    });
                }
                Some(Err(e)) => {
                    ui.label(egui::RichText::new(e).color(t.destructive));
                }
                Some(Ok(d)) => {
                    let verb = if delete { "excluídas" } else { "alteradas" };
                    let color = if d.count == 0 {
                        t.muted_foreground
                    } else if delete {
                        t.destructive
                    } else {
                        t.warning
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "{} linha(s) seriam {verb}",
                            palette::format_count(d.count as i64)
                        ))
                        .size(18.0)
                        .color(color),
                    );
                    if !d.sample.rows.is_empty() {
                        let what = if d.after {
                            "como ficariam (o comando rodou numa transação desfeita)"
                        } else {
                            "como estão agora"
                        };
                        let shown = d.sample.rows.len();
                        ui.label(
                            egui::RichText::new(format!("{shown} primeira(s), {what}"))
                                .size(11.0)
                                .color(t.subtle),
                        );
                        ui.add_space(4.0);
                        preview_rows(ui, tab.id, &d.sample);
                    }
                }
            }
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ready = matches!(&p.data, Some(Ok(_)));
                let label = if delete { "Excluir" } else { "Executar" };
                let button = if delete {
                    danger_button(label)
                } else {
                    theme::primary_button(Some(icon::PLAY), label)
                };
                if ui.add_enabled(ready && !s.cfg.read_only, button).clicked() {
                    run = true;
                }
                if ui.add(theme::outline_button("Cancelar")).clicked() {
                    cancel = true;
                }
            });
        });
    if run {
        let p = tab.preview.take().expect("open preview");
        run_now(tab, s, p.sql, RunMode::RunPreviewed);
    } else if cancel || modal.should_close() {
        tab.preview = None;
    }
}

/// A small read-only table for dialogs.
fn preview_rows(ui: &mut egui::Ui, tab_id: u64, res: &QueryResult) {
    let t = theme::tokens();
    egui::ScrollArea::both()
        .id_salt(("preview_rows", tab_id))
        .auto_shrink([false, true])
        .max_height(260.0)
        .show(ui, |ui| {
            egui::Grid::new(("preview_grid", tab_id))
                .striped(true)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for c in &res.columns {
                        ui.label(egui::RichText::new(c).size(11.0).color(t.subtle));
                    }
                    ui.end_row();
                    for row in &res.rows {
                        for v in row {
                            let text = match v {
                                Some(v) => egui::RichText::new(v.chars().take(60).collect::<String>())
                                    .monospace()
                                    .size(12.0)
                                    .color(t.foreground),
                                None => egui::RichText::new("NULL")
                                    .monospace()
                                    .size(12.0)
                                    .italics()
                                    .color(t.subtle),
                            };
                            ui.label(text);
                        }
                        ui.end_row();
                    }
                });
        });
}

/// Formats the selected chars of `text` (or all of it) as SQL.
fn format_in_place(text: &mut String, selected: Option<(usize, usize)>, kind: crate::db::Kind) {
    let byte = |text: &str, ci: usize| text.char_indices().nth(ci).map_or(text.len(), |(b, _)| b);
    match selected {
        Some((a, b)) => {
            let (a, b) = (byte(text, a), byte(text, b));
            let formatted = sql::format_sql(kind, &text[a..b]);
            text.replace_range(a..b, &formatted);
        }
        None => *text = sql::format_sql(kind, text),
    }
}

/// "Are you sure" for scripts with DROP / TRUNCATE / ALTER or DELETE / UPDATE without WHERE.
fn confirm_run(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let Some(sql) = tab.confirm_run.clone() else {
        return;
    };
    let t = theme::tokens();
    let risk = sql::script_risk(&sql);
    let (mut go, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new(("confirm_run", tab.id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(440.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::WARNING).size(16.0).color(t.destructive));
                ui.label(
                    egui::RichText::new("Comando destrutivo")
                        .size(15.0)
                        .color(t.foreground),
                );
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("Em {}:", s.cfg.name)).color(t.muted_foreground));
            for d in &risk.dangers {
                ui.label(
                    egui::RichText::new(format!("• {d}"))
                        .monospace()
                        .size(12.0)
                        .color(t.destructive),
                );
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Isso pode apagar ou alterar muitos dados e não dá para desfazer.")
                    .color(t.muted_foreground),
            );
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                go = ui.add(danger_button("Executar mesmo assim")).clicked();
                cancel = ui.add(theme::outline_button("Cancelar")).clicked();
            });
        });
    if go {
        tab.confirm_run = None;
        app::run_sql(tab, s, sql);
    } else if cancel || modal.should_close() {
        tab.confirm_run = None;
    }
}

/// Red button for the destructive choice in a dialog. Never the keyboard default: Enter does
/// nothing in those dialogs, only a click confirms.
fn danger_button(label: &str) -> egui::Button<'static> {
    let t = theme::tokens();
    egui::Button::new(egui::RichText::new(label.to_string()).color(t.foreground)).fill(t.destructive)
}

/// How often the activity tab refreshes itself while "Atualizar automaticamente" is on.
const ACTIVITY_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Sessions on the server, with buttons to cancel the selected one's query or close it.
fn activity_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let t = theme::tokens();
    let busy = tab.busy();
    // Session id of the selected row (first column), when it is a number.
    let target = tab.selected.and_then(|(row, _)| {
        tab.result
            .as_ref()?
            .rows
            .get(row)?
            .first()?
            .as_deref()?
            .parse::<i64>()
            .ok()
    });
    // The statement of the selected row ("consultas lentas"), to open in the editor.
    let selected_query = tab.selected.and_then(|(row, _)| {
        let res = tab.result.as_ref()?;
        let col = res.columns.iter().position(|c| c == "consulta")?;
        res.rows.get(row)?.get(col)?.clone()
    });
    let TabKind::Activity(st) = &mut tab.kind else {
        return;
    };
    let mut refresh = false;
    let mut switched = false;
    let mut reset_stats = false;
    egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::PULSE).size(15.0).color(t.muted_foreground));
                ui.spacing_mut().item_spacing.x = 0.0;
                for (view, label, tip) in [
                    (ActivityView::Sessions, "Sessões", "Quem está conectado e o que está executando"),
                    (ActivityView::Locks, "Bloqueios", "Sessões esperando por travas de outra (a primeira coluna é quem bloqueia)"),
                    (ActivityView::Slow, "Consultas lentas", "O que mais consumiu tempo no servidor desde a última vez que as estatísticas foram zeradas"),
                    (ActivityView::Server, "Servidor", "Versão, tamanho, conexões, cache, maiores tabelas e índices sem uso"),
                ] {
                    let on = st.view == view;
                    let color = if on { t.foreground } else { t.muted_foreground };
                    if ui
                        .add(egui::Button::new(egui::RichText::new(label).color(color)).selected(on))
                        .on_hover_text(tip)
                        .clicked()
                        && !on
                    {
                        st.view = view;
                        switched = true;
                    }
                }
                ui.spacing_mut().item_spacing.x = 8.0;
                if busy {
                    ui.spinner();
                }
                let view = st.view;
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let refresh_btn = theme::icon_text(
                        icon::ARROWS_CLOCKWISE,
                        "Atualizar",
                        t.muted_foreground,
                        t.foreground,
                    );
                    if ui.add_enabled(!busy, egui::Button::new(refresh_btn)).clicked() {
                        refresh = true;
                    }
                    if view == ActivityView::Server {
                        return;
                    }
                    ui.checkbox(&mut st.auto_refresh, "a cada 2 s");
                    ui.separator();
                    if view == ActivityView::Slow {
                        if ui
                            .add_enabled(!busy, egui::Button::new(theme::icon_text(
                                icon::ERASER,
                                "Zerar estatísticas",
                                t.muted_foreground,
                                t.foreground,
                            )))
                            .on_hover_text("Recomeça a contagem: útil para medir só o que acontece daqui em diante")
                            .clicked()
                        {
                            reset_stats = true;
                        }
                        if ui
                            .add_enabled(selected_query.is_some(), egui::Button::new(theme::icon_text(
                                icon::CODE,
                                "Abrir no editor",
                                t.muted_foreground,
                                t.foreground,
                            )))
                            .on_disabled_hover_text("Selecione uma consulta")
                            .clicked()
                            && let Some(q) = &selected_query
                        {
                            cx.request = Some(Request::Sql(q.clone()));
                        }
                        return;
                    }
                    let end =
                        theme::icon_text(icon::PLUGS, "Encerrar conexão", t.muted_foreground, t.foreground);
                    if ui
                        .add_enabled(target.is_some() && !busy, egui::Button::new(end))
                        .on_disabled_hover_text("Selecione uma sessão")
                        .clicked()
                    {
                        st.confirm_kill = target.map(|id| (id, true));
                    }
                    let stop =
                        theme::icon_text(icon::STOP, "Cancelar consulta", t.muted_foreground, t.foreground);
                    if ui
                        .add_enabled(target.is_some() && !busy, egui::Button::new(stop))
                        .on_disabled_hover_text("Selecione uma sessão")
                        .clicked()
                    {
                        st.confirm_kill = target.map(|id| (id, false));
                    }
                });
            });
        });
    if switched {
        tab.result = None;
        tab.selected = None;
        tab.anchor = None;
        tab.error = None;
        app::refresh_activity(tab, s);
    }
    // The server overview is a panel, refreshed by hand.
    if let TabKind::Activity(st) = &tab.kind
        && st.view == ActivityView::Server
    {
        if refresh && !switched {
            app::refresh_activity(tab, s);
        }
        if let TabKind::Activity(st) = &tab.kind {
            server_panel(ui, tab.id, st.server.as_ref(), tab.busy());
        }
        return;
    }
    if switched {
        result_area(ui, tab, cx, false);
        return;
    }
    if reset_stats {
        app::reset_query_stats(tab, s);
    }
    let TabKind::Activity(st) = &mut tab.kind else {
        return;
    };
    // While the confirmation is open the list stays still, so the selection can't move under it.
    if st.auto_refresh && st.confirm_kill.is_none() && !busy && !reset_stats {
        let due = st.last_refresh.is_none_or(|at| at.elapsed() >= ACTIVITY_EVERY);
        if due {
            refresh = true;
        } else if let Some(at) = st.last_refresh {
            ui.ctx()
                .request_repaint_after(ACTIVITY_EVERY.saturating_sub(at.elapsed()));
        }
    }
    if let Some((id, terminate)) = st.confirm_kill {
        match confirm_kill(ui.ctx(), tab.id, id, terminate) {
            Some(true) => app::kill_session(tab, s),
            Some(false) => {
                if let TabKind::Activity(st) = &mut tab.kind {
                    st.confirm_kill = None;
                }
            }
            None => {}
        }
    } else if refresh {
        app::refresh_activity(tab, s);
    }
    if let TabKind::Activity(st) = &tab.kind
        && let Some(e) = &st.kill_error
    {
        error_banner(ui, e);
    }
    result_area(ui, tab, cx, false);
}

/// Every query run, newest first: search, filter, look at one, open it in an editor.
fn history_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let t = theme::tokens();
    let conn = cx.s.cfg.name.clone();
    let TabKind::History(st) = &mut tab.kind else {
        return;
    };
    egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    theme::icon(icon::CLOCK_COUNTER_CLOCKWISE)
                        .size(15.0)
                        .color(t.muted_foreground),
                );
                ui.label(
                    egui::RichText::new("Histórico de consultas")
                        .size(15.0)
                        .color(t.foreground),
                );
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let r = ui.add(
                    TextEdit::singleline(&mut st.query)
                        .hint_text("Buscar no texto das consultas")
                        .desired_width(320.0),
                );
                if std::mem::take(&mut st.focus) {
                    r.request_focus();
                }
                ui.checkbox(&mut st.all_conns, "todas as conexões");
                ui.checkbox(&mut st.only_errors, "só as que falharam");
            });
        });
    let needle = st.query.to_lowercase();
    let list: Vec<&Entry> = cx
        .history
        .iter()
        .rev()
        .filter(|e| st.all_conns || e.conn == conn)
        .filter(|e| !st.only_errors || !e.ok)
        .filter(|e| needle.is_empty() || e.sql.to_lowercase().contains(&needle))
        .collect();
    let picked = st
        .selected
        .as_ref()
        .and_then(|(c, q)| list.iter().find(|e| e.conn == *c && e.sql == *q).copied());

    // The chosen one, whole, with what can be done with it.
    if let Some(e) = picked {
        egui::Panel::bottom(egui::Id::new(("history_detail", tab.id)))
            .resizable(true)
            .default_size(200.0)
            .frame(
                egui::Frame::new()
                    .fill(t.sidebar)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let when = history::ago(e.at, history::now());
                    let status = if e.ok {
                        format!("{} ms", e.ms)
                    } else {
                        "falhou".to_string()
                    };
                    ui.label(
                        egui::RichText::new(format!("{} · {when} · {status}", e.conn))
                            .size(12.0)
                            .color(t.subtle),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(theme::primary_button(Some(icon::CODE), "Abrir no editor"))
                            .clicked()
                        {
                            cx.request = Some(Request::Sql(e.sql.clone()));
                        }
                        if ui
                            .button(theme::icon_text(
                                icon::COPY,
                                "Copiar",
                                t.muted_foreground,
                                t.foreground,
                            ))
                            .clicked()
                        {
                            ui.ctx().copy_text(e.sql.clone());
                        }
                        if ui
                            .button(theme::icon_text(
                                icon::TRASH,
                                "Esquecer",
                                t.muted_foreground,
                                t.foreground,
                            ))
                            .on_hover_text("Tira esta consulta do histórico")
                            .clicked()
                        {
                            cx.request = Some(Request::ForgetHistory(e.conn.clone(), e.sql.clone()));
                        }
                    });
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let job = syntax::highlight(&e.sql, egui::FontId::monospace(12.5));
                        ui.add(egui::Label::new(job).selectable(true));
                    });
            });
    }

    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(format!("{} consulta(s)", list.len()))
                .size(11.0)
                .color(t.subtle),
        );
    });
    // Only the rows on screen are laid out: the history can hold thousands.
    let row_h = 26.0;
    let now = history::now();
    let mut select: Option<(String, String)> = None;
    let mut open: Option<String> = None;
    egui::ScrollArea::vertical()
        .id_salt(("history_list", tab.id))
        .auto_shrink([false, false])
        .show_rows(ui, row_h, list.len(), |ui, range| {
            for e in &list[range] {
                let selected = picked.is_some_and(|p| std::ptr::eq(p, *e));
                let first = e.sql.split_whitespace().collect::<Vec<_>>().join(" ");
                let color = if e.ok { t.foreground } else { t.destructive };
                let mut right = history::ago(e.at, now);
                if st.all_conns {
                    right = format!("{} · {right}", e.conn);
                }
                let r = ui.add(
                    egui::Button::selectable(
                        selected,
                        egui::RichText::new(first).monospace().size(12.0).color(color),
                    )
                    .truncate()
                    .min_size(egui::vec2(ui.available_width(), row_h - 2.0))
                    .right_text(egui::RichText::new(right).size(11.0).color(t.subtle)),
                );
                if r.clicked() {
                    select = Some((e.conn.clone(), e.sql.clone()));
                }
                if r.double_clicked() {
                    open = Some(e.sql.clone());
                }
            }
        });
    if select.is_some() {
        st.selected = select;
    }
    if let Some(sql) = open {
        cx.request = Some(Request::Sql(sql));
    }
}

/// "Buscar valor em todas as tabelas": the term, options, progress and where it was found.
fn search_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let t = theme::tokens();
    let TabKind::Search(st) = &mut tab.kind else {
        return;
    };
    let (mut start, mut cancel) = (false, false);
    let mut open: Option<(String, String, String)> = None;
    egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::MAGNIFYING_GLASS).size(15.0).color(t.muted_foreground));
                ui.label(egui::RichText::new("Buscar valor em todas as tabelas").size(15.0).color(t.foreground));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().interact_size.y = 28.0;
                let r = ui.add_enabled(
                    !st.running,
                    TextEdit::singleline(&mut st.term)
                        .hint_text("e-mail, CPF, código, id…  (Enter busca)")
                        .font(egui::FontId::monospace(13.0))
                        .margin(egui::Margin::symmetric(8, 6))
                        .desired_width(360.0),
                );
                if std::mem::take(&mut st.focus) {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    start = true;
                }
                if st.running {
                    if ui.add(theme::outline_button("Cancelar")).clicked() {
                        cancel = true;
                    }
                } else if ui
                    .add_enabled(!st.term.trim().is_empty(), theme::primary_button(Some(icon::MAGNIFYING_GLASS), "Buscar"))
                    .clicked()
                {
                    start = true;
                }
                ui.add_enabled(!st.running, egui::Checkbox::new(&mut st.exact, "valor exato"))
                    .on_hover_text("Desmarcado: procura o texto dentro dos valores (qualquer caixa)");
                ui.add_enabled(
                    !st.running,
                    egui::Checkbox::new(&mut st.include_big, "incluir tabelas grandes"),
                )
                .on_hover_text("Tabelas com mais de 1 milhão de linhas: cada uma é lida inteira e leva segundos");
            });
            ui.label(
                egui::RichText::new(
                    "Lê as tabelas uma a uma (só colunas onde o valor caberia). Enquanto isso, as outras abas \
                     desta conexão esperam.",
                )
                .size(11.0)
                .color(t.subtle),
            );
            if st.running || st.total > 0 {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let frac = if st.total == 0 { 0.0 } else { st.done as f32 / st.total as f32 };
                    ui.add(egui::ProgressBar::new(frac).desired_width(240.0).desired_height(6.0).fill(t.lime));
                    let mut text = format!(
                        "{} de {} tabelas · encontrado em {}",
                        st.done,
                        st.total,
                        st.hits.len()
                    );
                    if st.skipped > 0 {
                        text.push_str(&format!(" · {} grandes puladas", st.skipped));
                    }
                    if !st.errors.is_empty() {
                        text.push_str(&format!(" · {} com erro", st.errors.len()));
                    }
                    let r = ui.label(egui::RichText::new(text).size(12.0).color(t.muted_foreground));
                    if !st.errors.is_empty() {
                        r.on_hover_text(st.errors.join("\n"));
                    }
                });
            }
        });

    egui::ScrollArea::vertical()
        .id_salt(("search_hits", tab.id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 4))
                .show(ui, |ui| {
                    if st.hits.is_empty() {
                        if !st.running && st.total > 0 {
                            ui.label(egui::RichText::new("Nada encontrado").color(t.subtle));
                        }
                        return;
                    }
                    egui::Grid::new(("search_grid", tab.id))
                        .num_columns(4)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            for h in ["tabela", "colunas", "linhas", ""] {
                                ui.label(egui::RichText::new(h).size(11.0).color(t.subtle));
                            }
                            ui.end_row();
                            for h in &st.hits {
                                ui.label(
                                    egui::RichText::new(format!("{}.{}", h.schema, h.table))
                                        .monospace()
                                        .color(t.foreground),
                                );
                                ui.label(
                                    egui::RichText::new(h.columns.join(", "))
                                        .monospace()
                                        .color(t.info),
                                );
                                let rows = if h.rows >= crate::search::ROWS_PER_TABLE {
                                    format!("{}+", h.rows)
                                } else {
                                    h.rows.to_string()
                                };
                                ui.label(egui::RichText::new(rows).monospace().color(t.number));
                                if ui
                                    .button(theme::icon_text(
                                        icon::TABLE,
                                        "Abrir",
                                        t.muted_foreground,
                                        t.foreground,
                                    ))
                                    .on_hover_text(egui::RichText::new(&h.filter).monospace())
                                    .clicked()
                                {
                                    open = Some((h.schema.clone(), h.table.clone(), h.filter.clone()));
                                }
                                ui.end_row();
                            }
                        });
                });
        });
    if start {
        app::start_search(tab, s);
    } else if cancel {
        app::cancel_search(tab, s);
    }
    if let Some((schema, table, filter)) = open {
        cx.request = Some(Request::OpenFiltered(schema, table, filter));
    }
}

/// A `.sql` file run statement by statement: progress while it goes, then where it stopped.
fn script_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let t = theme::tokens();
    let TabKind::Script(st) = &mut tab.kind else {
        return;
    };
    let (mut start, mut cancel) = (false, false);
    egui::ScrollArea::vertical()
        .id_salt(("script", tab.id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .fill(t.background)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::icon(icon::PLAY).size(15.0).color(t.muted_foreground));
                        ui.label(egui::RichText::new("Executar arquivo .sql").size(15.0).color(t.foreground));
                    });
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(st.path.display().to_string())
                            .monospace()
                            .color(t.foreground),
                    );
                    let here = match s.cfg.kind {
                        crate::db::Kind::Postgres => format!("{} · {}", s.current_db, s.current_schema),
                        crate::db::Kind::MySql => s.current_db.clone(),
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · em {} ({})",
                            super::palette::format_bytes(st.size as i64),
                            here,
                            s.cfg.name
                        ))
                        .size(12.0)
                        .color(t.muted_foreground),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().interact_size.y = 28.0;
                        if st.running {
                            if ui.add(theme::outline_button("Cancelar")).clicked() {
                                cancel = true;
                            }
                        } else {
                            let label = if st.outcome.is_some() { "Executar de novo" } else { "Executar" };
                            if ui.add(theme::primary_button(Some(icon::PLAY), label)).clicked() {
                                start = true;
                            }
                        }
                        let hint = match s.cfg.kind {
                            crate::db::Kind::Postgres => {
                                "Tudo ou nada: no primeiro erro (ou ao cancelar) nada do arquivo fica no banco"
                            }
                            crate::db::Kind::MySql => {
                                "No MySQL, cada CREATE/ALTER/DROP confirma o que veio antes: num erro, só volta \
                                 atrás o que rodou depois do último deles"
                            }
                        };
                        ui.add_enabled(!st.running, egui::Checkbox::new(&mut st.transaction, "numa transação"))
                            .on_hover_text(hint);
                    });
                    ui.label(
                        egui::RichText::new(
                            "Lê o arquivo aos poucos e para no primeiro erro. Entende DELIMITER (MySQL), blocos \
                             $$ e COPY … FROM stdin do pg_dump; comandos do psql (\\connect…) são pulados.",
                        )
                        .size(11.0)
                        .color(t.subtle),
                    );
                    if !st.running && st.outcome.is_none() {
                        return;
                    }
                    ui.add_space(12.0);
                    let p = st.progress;
                    let frac = if st.size == 0 { 1.0 } else { (p.bytes as f32 / st.size as f32).min(1.0) };
                    ui.horizontal(|ui| {
                        let fill = match &st.outcome {
                            Some(Some(_)) => t.destructive,
                            _ => t.lime,
                        };
                        ui.add(egui::ProgressBar::new(frac).desired_width(320.0).desired_height(6.0).fill(fill));
                        let took = st.took.or_else(|| st.started.map(|at| at.elapsed())).unwrap_or_default();
                        let plural = |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
                        let mut text = format!(
                            "{:.0}% · {} · linha {} · {:.1}s",
                            frac * 100.0,
                            plural(p.statements, "comando", "comandos"),
                            p.line,
                            took.as_secs_f32()
                        );
                        if p.copied > 0 {
                            text.push_str(&format!(" · {} via COPY", plural(p.copied, "linha", "linhas")));
                        }
                        if p.skipped > 0 {
                            let skipped = plural(p.skipped, "comando do psql pulado", "comandos do psql pulados");
                            text.push_str(&format!(" · {skipped}"));
                        }
                        ui.label(egui::RichText::new(text).size(12.0).color(t.muted_foreground));
                    });
                    if st.running {
                        // The worker reports a few times a second; keep the clock moving.
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                        return;
                    }
                    ui.add_space(8.0);
                    match &st.outcome {
                        Some(None) => {
                            ui.horizontal(|ui| {
                                ui.label(theme::icon(icon::CHECK).size(16.0).color(t.lime));
                                let done = if st.transaction { "Arquivo executado e confirmado" } else { "Arquivo executado" };
                                ui.label(egui::RichText::new(done).color(t.foreground));
                            });
                        }
                        Some(Some(e)) => {
                            let undone = if st.transaction {
                                " Nada do arquivo ficou no banco (transação desfeita)."
                            } else {
                                " O que rodou antes disso continua no banco."
                            };
                            let title = if e.cancelled {
                                format!("Cancelado na linha {}.{undone}", e.line)
                            } else {
                                format!("Erro na linha {}.{undone}", e.line)
                            };
                            ui.horizontal(|ui| {
                                ui.label(theme::icon(icon::WARNING).size(16.0).color(t.destructive));
                                ui.label(egui::RichText::new(title).color(t.foreground));
                            });
                            if !e.cancelled {
                                ui.add_space(4.0);
                                error_banner(ui, &e.message);
                            }
                            if !e.statement.is_empty() {
                                ui.add_space(4.0);
                                egui::Frame::new()
                                    .fill(t.card)
                                    .stroke(egui::Stroke::new(1.0, t.border))
                                    .inner_margin(egui::Margin::same(8))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(
                                            egui::RichText::new(&e.statement)
                                                .monospace()
                                                .size(12.0)
                                                .color(t.foreground),
                                        );
                                    });
                            }
                        }
                        None => {}
                    }
                });
        });
    if start {
        app::start_script(tab, s);
    } else if cancel {
        app::cancel_script(tab, s);
    }
}

/// "Servidor": headline numbers as tiles, then the biggest tables and the least used indexes.
fn server_panel(
    ui: &mut egui::Ui,
    tab_id: u64,
    info: Option<&Result<crate::db::ServerInfo, String>>,
    busy: bool,
) {
    let t = theme::tokens();
    let info = match info {
        None => {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                if busy {
                    ui.spinner();
                }
            });
            return;
        }
        Some(Err(e)) => return error_banner(ui, e),
        Some(Ok(i)) => i,
    };
    egui::ScrollArea::vertical()
        .id_salt(("server", tab_id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                        for (label, value) in &info.facts {
                            egui::Frame::new()
                                .fill(t.sidebar)
                                .stroke(egui::Stroke::new(1.0, t.border))
                                .inner_margin(egui::Margin::symmetric(12, 8))
                                .show(ui, |ui| {
                                    ui.set_min_width(140.0);
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(label).size(11.0).color(t.subtle));
                                        // The version string can be long (MySQL adds the build).
                                        let v: String = value.chars().take(28).collect();
                                        ui.label(egui::RichText::new(v).size(18.0).color(t.foreground))
                                            .on_hover_text(value);
                                    });
                                });
                        }
                    });
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("MAIORES TABELAS").size(11.0).color(t.subtle));
                    ui.add_space(4.0);
                    preview_rows(ui, tab_id ^ 1, &info.largest);
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("ÍNDICES MENOS USADOS")
                            .size(11.0)
                            .color(t.subtle),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Leituras desde que as estatísticas começaram (no MySQL, desde que a tabela foi \
                             aberta; vazio = sem dados). Índice pouco usado ocupa espaço e deixa as escritas \
                             mais lentas, mas os que apoiam chave estrangeira costumam ser necessários.",
                        )
                        .size(11.0)
                        .color(t.subtle),
                    );
                    ui.add_space(4.0);
                    if info.indexes.rows.is_empty() {
                        ui.label(
                            egui::RichText::new("Nenhum índice além de chaves primárias e únicos")
                                .color(t.muted_foreground),
                        );
                    } else {
                        preview_rows(ui, tab_id ^ 2, &info.indexes);
                    }
                });
        });
}

/// `Some(true)` to go ahead, `Some(false)` to give up, `None` while the dialog stays open.
fn confirm_kill(ctx: &egui::Context, tab_id: u64, id: i64, terminate: bool) -> Option<bool> {
    let t = theme::tokens();
    let mut answer = None;
    let (title, text, button) = if terminate {
        (
            "Encerrar conexão",
            "A sessão é desconectada; uma transação aberta nela é desfeita.",
            "Encerrar",
        )
    } else {
        (
            "Cancelar consulta",
            "O comando em andamento nessa sessão é interrompido; a conexão continua aberta.",
            "Cancelar consulta",
        )
    };
    let modal = egui::Modal::new(egui::Id::new(("confirm_kill", tab_id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(400.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::WARNING).size(16.0).color(t.destructive));
                ui.label(
                    egui::RichText::new(format!("{title} — sessão {id}"))
                        .size(15.0)
                        .color(t.foreground),
                );
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new(text).color(t.muted_foreground));
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(danger_button(button)).clicked() {
                    answer = Some(true);
                }
                if ui.add(theme::outline_button("Voltar")).clicked() {
                    answer = Some(false);
                }
            });
        });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    answer
}

fn table_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let search_id = egui::Id::new(("table_search", tab.id));
    // Ctrl+S / Ctrl+Z work on pending changes, unless a text field wants the keys.
    if !ui.ctx().egui_wants_keyboard_input() {
        let (save, undo) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND, Key::S),
                i.consume_key(egui::Modifiers::COMMAND, Key::Z),
            )
        });
        if let TabKind::Table(ts) = &mut tab.kind {
            if save && !ts.changes.is_empty() {
                ts.confirm_save = true;
            }
            if undo {
                ts.changes.pop();
            }
        }
    }
    let tab_id = tab.id;
    let mut reload_now = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F5));
    let mut add_record = false;
    let status = status_text(tab, s.cfg.read_only);
    let busy = tab.busy();
    let rows = tab.result.as_ref().map_or(0, |r| r.rows.len());
    let full = rows >= page_size();
    let can_export = !busy && tab.result.as_ref().is_some_and(|r| !r.columns.is_empty());
    let mut export = None;
    let mut show_structure = false;
    let mut import_csv = false;
    let TabKind::Table(ts) = &mut tab.kind else {
        return;
    };

    let t = theme::tokens();
    let bar = egui::Frame::new()
        .fill(t.sidebar)
        .inner_margin(egui::Margin::symmetric(8, 2));
    egui::Panel::bottom(egui::Id::new(("pager", tab.id)))
        .frame(bar)
        .show(ui, |ui| {
            ui.spacing_mut().interact_size.y = 20.0;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(status).small().color(t.muted_foreground));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let next = egui::Button::new(theme::icon(icon::CARET_RIGHT)).frame(false);
                    if ui.add_enabled(full, next).clicked() {
                        ts.page += 1;
                        reload_now = true;
                    }
                    let first = ts.page * page_size() + 1;
                    let range = if rows == 0 {
                        "0".to_string()
                    } else {
                        format!("{first}–{}", first + rows - 1)
                    };
                    // Exact total when we have it, the planner's estimate otherwise.
                    let of = match (ts.total, ts.estimate) {
                        (Some(n), _) => format!(" de {}", palette::format_count(n)),
                        (None, Some(n)) if ts.filter.trim().is_empty() => {
                            format!(" de ~{}", palette::format_count(n))
                        }
                        _ => String::new(),
                    };
                    ui.label(
                        egui::RichText::new(format!("{range}{of}"))
                            .small()
                            .color(t.muted_foreground),
                    );
                    let prev = egui::Button::new(theme::icon(icon::CARET_LEFT)).frame(false);
                    if ui.add_enabled(ts.page > 0, prev).clicked() {
                        ts.page -= 1;
                        reload_now = true;
                    }
                    if busy {
                        busy_indicator(ui, s);
                    }
                });
            });
        });

    let toolbar = egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            // Roomier than the rest of the UI: the filter is where you type the most.
            ui.spacing_mut().interact_size.y = 28.0;
            ui.horizontal(|ui| {
                // Search: the term in any column, on top of the WHERE filter.
                let search = ui.add(
                    TextEdit::singleline(&mut ts.search)
                        .id(search_id)
                        .hint_text("Buscar em todas as colunas  (Ctrl+F)")
                        .margin(egui::Margin::symmetric(8, 6))
                        .desired_width(200.0),
                );
                if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    ts.page = 0;
                    reload_now = true;
                }
                match filters_menu(ui, tab_id, cx.filters, &s.cfg.id, ts) {
                    Some(FilterAction::Apply(f)) => {
                        ts.filter = f;
                        ts.page = 0;
                        reload_now = true;
                    }
                    Some(FilterAction::Save(name)) => {
                        cx.request = Some(Request::SaveFilter(
                            ts.schema.clone(),
                            ts.table.clone(),
                            name,
                            ts.filter.clone(),
                        ));
                    }
                    Some(FilterAction::Delete(id)) => cx.request = Some(Request::DeleteFilter(id)),
                    None => {}
                }
                ui.label(
                    egui::RichText::new("WHERE")
                        .monospace()
                        .color(syntax::keyword_color()),
                );
                // The buttons on the right were measured last frame; the filter takes the rest.
                // A guess here (instead of a measurement) made the row overflow, the panel grow,
                // and the whole UI repaint forever.
                let right_id = egui::Id::new(("toolbar_right", tab_id));
                let right_w = ui.data(|d| d.get_temp::<f32>(right_id)).unwrap_or(440.0);
                let w = (ui.available_width() - right_w - 8.0).max(120.0);
                let r = ui.add(
                    TextEdit::singleline(&mut ts.filter)
                        .hint_text("id > 10 AND nome LIKE 'a%'   (Enter aplica)")
                        .font(egui::FontId::monospace(13.0))
                        .margin(egui::Margin::symmetric(8, 6))
                        .layouter(&mut syntax::sql_layouter(13.0))
                        .desired_width(w),
                );
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    ts.page = 0;
                    reload_now = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let start = ui.cursor().right();
                    add_record = ui
                        .add_enabled(
                            !s.cfg.read_only,
                            theme::primary_button(Some(icon::PLUS), "Registro"),
                        )
                        .on_disabled_hover_text("Conexão somente leitura")
                        .clicked();
                    if ui
                        .button(theme::icon(icon::ARROWS_CLOCKWISE))
                        .on_hover_text("Recarregar (F5)")
                        .clicked()
                    {
                        reload_now = true;
                    }
                    detail_toggle(ui, cx.show_detail);
                    export = export_menu(ui, true, can_export);
                    import_csv = ui
                        .add_enabled(
                            !s.cfg.read_only,
                            egui::Button::new(theme::icon(icon::UPLOAD_SIMPLE).color(t.muted_foreground)),
                        )
                        .on_hover_text("Importar CSV (ou arraste um arquivo .csv para a janela)")
                        .on_disabled_hover_text("Conexão somente leitura")
                        .clicked();
                    let structure = theme::icon_text(
                        icon::TREE_STRUCTURE,
                        "Estrutura",
                        t.muted_foreground,
                        t.foreground,
                    );
                    show_structure = ui
                        .button(structure)
                        .on_hover_text("Colunas, índices e DDL")
                        .clicked();
                    let used = start - ui.cursor().right();
                    ui.data_mut(|d| d.insert_temp(right_id, used));
                });
            })
        });
    if show_structure {
        cx.request = Some(Request::Structure(ts.schema.clone(), ts.table.clone()));
    }
    let r = toolbar.response.rect;
    ui.painter()
        .hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, t.border));
    if let Some((format, whole)) = export {
        app::start_export(tab, s, format, whole);
    }
    // A .csv dropped on the window opens the import dialog with it.
    let dropped = ui.ctx().input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path())
            .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("csv")))
            .map(|p| p.display().to_string())
    });
    if (import_csv || dropped.is_some()) && !s.cfg.read_only {
        app::open_import(tab, dropped);
    }
    if add_record {
        app::open_insert_form(tab);
    } else if reload_now {
        app::reload(tab, s);
    }
    export_banner(ui, tab);
    changes_bar(ui, tab, s);
    result_area(ui, tab, cx, true);
    confirm_save(ui.ctx(), tab, s);
    insert_form(ui.ctx(), tab, s);
    import_dialog(ui.ctx(), tab, s);
}

fn import_dialog(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let (id, busy) = (tab.id, tab.busy());
    let TabKind::Table(ts) = &mut tab.kind else { return };
    if ts.import.is_none() {
        return;
    }
    app::refresh_import_preview(ts);
    let name = format!("{}.{}", ts.schema, ts.table);
    let cols: Vec<String> = ts.cols.iter().map(|c| c.name.clone()).collect();
    let Some(form) = &mut ts.import else { return };
    match import_form::import_form(ctx, id, &name, &cols, form, busy) {
        Some(ImportAction::Submit) => app::submit_import(tab, s),
        Some(ImportAction::Cancel) => ts.import = None,
        None => {}
    }
}

fn insert_form(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let id = tab.id;
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let name = format!("{}.{}", ts.schema, ts.table);
    let Some(fields) = &mut ts.insert else { return };
    match structure::insert_form(ctx, id, &name, fields) {
        Some(InsertAction::Submit) => app::submit_insert(tab, s),
        Some(InsertAction::Cancel) => ts.insert = None,
        None => {}
    }
}

/// "N alterações pendentes · Desfazer · Descartar · Salvar" while there are unsaved edits.
fn changes_bar(ui: &mut egui::Ui, tab: &mut Tab, s: &Session) {
    let Some(ts) = app::editable_mut(&mut tab.kind, &mut tab.source) else {
        return;
    };
    if ts.changes.is_empty() {
        return;
    }
    let t = theme::tokens();
    let (edits, deletes) = ts.change_counts();
    let (mut undo, mut discard, mut save) = (false, false, false);
    egui::Frame::new()
        .fill(theme::mix(t.warning, 0.10, t.background))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::PENCIL_SIMPLE).color(t.warning));
                let mut parts = Vec::new();
                if edits > 0 {
                    parts.push(format!("{edits} edição(ões)"));
                }
                if deletes > 0 {
                    parts.push(format!("{deletes} exclusão(ões)"));
                }
                ui.label(
                    egui::RichText::new(format!("{} pendente(s), ainda não salvas", parts.join(" e ")))
                        .color(t.foreground),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    save = ui
                        .add(theme::primary_button(Some(icon::FLOPPY_DISK), "Salvar"))
                        .on_hover_text("Ctrl+S")
                        .clicked();
                    discard = ui.add(theme::outline_button("Descartar")).clicked();
                    undo = ui
                        .button(theme::icon_text(
                            icon::ARROW_COUNTER_CLOCKWISE,
                            "Desfazer",
                            t.muted_foreground,
                            t.foreground,
                        ))
                        .on_hover_text("Ctrl+Z")
                        .clicked();
                });
            });
        });
    let _ = s;
    if undo {
        app::undo_change(tab);
    } else if discard {
        app::discard_changes(tab);
    } else if save && let Some(ts) = app::editable_mut(&mut tab.kind, &mut tab.source) {
        ts.confirm_save = true;
    }
}

/// Summary before saving: what will change, and the exact SQL. Deletions make the confirm
/// button red; Enter never confirms here.
fn confirm_save(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let Some(ts) = app::editable_mut(&mut tab.kind, &mut tab.source) else {
        return;
    };
    if !ts.confirm_save {
        return;
    }
    if ts.changes.is_empty() {
        ts.confirm_save = false;
        return;
    }
    let t = theme::tokens();
    let k = s.cfg.kind;
    let (edits, deletes) = ts.change_counts();
    let stmts = ts.change_statements(k);
    let (mut go, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new(("confirm_save", tab.id))).frame(theme::modal_frame(16)).show(ctx, |ui| {
        ui.set_width(520.0);
        ui.horizontal(|ui| {
            ui.label(theme::icon(icon::FLOPPY_DISK).size(16.0).color(if deletes > 0 { t.destructive } else { t.lime }));
            ui.label(egui::RichText::new(format!("Salvar {} alteração(ões)?", stmts.len())).size(15.0).color(t.foreground));
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!(
                "Em {}.{}: {edits} edição(ões), {deletes} exclusão(ões). Tudo numa transação: se uma falhar, nada é salvo.",
                ts.schema, ts.table
            ))
            .color(t.muted_foreground),
        );
        ui.add_space(8.0);
        egui::Frame::new().fill(t.sidebar).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                for st in &stmts {
                    let job = syntax::highlight(&format!("{};", st.render(k)), egui::FontId::monospace(12.0));
                    ui.add(egui::Label::new(job).wrap());
                }
            });
        });
        ui.add_space(16.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            go = if deletes > 0 {
                ui.add(danger_button("Salvar e excluir")).clicked()
            } else {
                ui.add(theme::primary_button(Some(icon::FLOPPY_DISK), "Salvar")).clicked()
            };
            cancel = ui.add(theme::outline_button("Voltar")).clicked();
        });
    });
    if go {
        app::save_changes(tab, s);
    } else if cancel || modal.should_close() {
        ts.confirm_save = false;
    }
}

/// Summary line for the pager: row count, timing and what the user can do with the grid.
fn status_text(tab: &Tab, read_only: bool) -> String {
    let Some(res) = &tab.result else {
        return String::new();
    };
    let hint = match &tab.kind {
        TabKind::Table(_) if read_only => " · conexão somente leitura",
        TabKind::Table(ts) if !ts.cols.is_empty() => {
            if ts.cols.iter().any(|c| c.pk) {
                " · duplo clique ou Enter edita · Ctrl+S salva"
            } else {
                " · sem chave primária: somente leitura"
            }
        }
        _ => "",
    };
    format!("{}{hint}", summary(res))
}

/// "Resultado 1 · 2 · 3" above the grid when a script returned several result sets.
fn result_sets_strip(ui: &mut egui::Ui, tab: &Tab) -> Option<usize> {
    let t = theme::tokens();
    let mut pick = None;
    egui::Frame::new()
        .fill(t.sidebar)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::horizontal()
                .id_salt(("sets", tab.id))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(egui::RichText::new("RESULTADOS").size(11.0).color(t.subtle));
                        for (i, set) in tab.sets.iter().enumerate() {
                            let shown = i == tab.set_shown;
                            let set = if shown {
                                tab.result.as_ref().unwrap_or(set)
                            } else {
                                set
                            };
                            let n = set.rows.len();
                            let label = if set.columns.is_empty() {
                                format!("#{} · {} afetada(s)", i + 1, set.affected.unwrap_or(0))
                            } else {
                                let unit = if n == 1 { "linha" } else { "linhas" };
                                format!("#{} · {} {unit}", i + 1, palette::compact_count(n as i64))
                            };
                            let color = if shown { t.foreground } else { t.muted_foreground };
                            let b = egui::Button::new(egui::RichText::new(label).size(12.0).color(color))
                                .selected(shown);
                            let tip = format!("{} coluna(s): {}", set.columns.len(), set.columns.join(", "));
                            if ui.add(b).on_hover_text(tip).clicked() {
                                pick = Some(i);
                            }
                        }
                    });
                });
        });
    pick
}

fn summary(res: &QueryResult) -> String {
    if res.columns.is_empty() {
        format!(
            "{} linha(s) afetada(s) · {} ms",
            res.affected.unwrap_or(0),
            res.elapsed.as_millis()
        )
    } else {
        let truncated = if res.truncated {
            " · resultado truncado"
        } else {
            ""
        };
        format!(
            "{} linha(s) · {} ms{truncated}",
            res.rows.len(),
            res.elapsed.as_millis()
        )
    }
}

fn result_area(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx, editable: bool) {
    let s = cx.s;
    stats_dialog(ui.ctx(), tab, s);
    if let Some(e) = fill_dialog(ui.ctx(), tab) {
        app::handle_grid_event(tab, s, e, true);
    }
    let t = theme::tokens();
    if let Some(e) = &tab.error {
        error_banner(ui, e);
    }
    if !tab.sets.is_empty()
        && let Some(i) = result_sets_strip(ui, tab)
    {
        tab.show_set(i);
    }
    let Some(res) = &tab.result else {
        if tab.busy() {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new("Carregando…").color(t.subtle));
            });
        } else if !editable && tab.error.is_none() {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.label(theme::icon(icon::PLAY).size(20.0).color(t.subtle));
                ui.label(
                    egui::RichText::new("Execute uma consulta para ver o resultado aqui").color(t.subtle),
                );
            });
        }
        return;
    };
    // Editor results can be charted (kept in egui memory per tab).
    let is_sql = matches!(tab.kind, TabKind::Sql { .. });
    // Editor results from one table say whether they can be edited, and why not.
    let edit_hint = match (&tab.kind, &tab.source) {
        (TabKind::Sql { .. }, Some(src)) if !src.cols.is_empty() => {
            let pk = src.pk_names();
            if s.cfg.read_only {
                None
            } else if pk.is_empty() {
                Some(format!("· {} não tem chave primária: somente leitura", src.table))
            } else if pk.iter().all(|p| res.columns.contains(p)) {
                Some(format!(
                    "· editável ({}) · duplo clique edita · Ctrl+S salva",
                    src.table
                ))
            } else {
                Some(format!("· inclua {} no SELECT para poder editar", pk.join(", ")))
            }
        }
        _ => None,
    };
    let chart_id = egui::Id::new(("chart", tab.id));
    let mut chart_spec: Option<chart::ChartSpec> = ui.data(|d| d.get_temp(chart_id)).flatten();
    let result_kinds = values::column_kinds(res, &[]);
    // A new result may not have the chosen columns any more: pick again.
    if let Some(c) = chart_spec
        && (c.x >= res.columns.len()
            || c.y >= res.columns.len()
            || result_kinds[c.y] != values::ValueKind::Number)
    {
        chart_spec = chart::default_spec(res, &result_kinds);
    }
    if !editable {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(8, 2))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(summary(res))
                            .small()
                            .color(t.muted_foreground),
                    );
                    if let Some(hint) = &edit_hint {
                        ui.label(egui::RichText::new(hint).small().color(t.subtle));
                    }
                    let chartable =
                        is_sql && chart::default_spec(res, &result_kinds).is_some() && !res.rows.is_empty();
                    if chartable {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let on = chart_spec.is_some();
                            let color = if on { t.lime } else { t.muted_foreground };
                            let b = egui::Button::new(theme::icon_text(
                                icon::CHART_BAR,
                                "Gráfico",
                                color,
                                t.foreground,
                            ))
                            .frame(false);
                            if ui
                                .add(b)
                                .on_hover_text("Barras ou linha de uma coluna numérica")
                                .clicked()
                            {
                                chart_spec = if on {
                                    None
                                } else {
                                    chart::default_spec(res, &result_kinds)
                                };
                            }
                        });
                    }
                });
            });
    }
    if res.columns.is_empty() {
        return;
    }
    let mut close_chart = false;
    if is_sql && let Some(spec) = &mut chart_spec {
        egui::Panel::top(egui::Id::new(("chart_panel", tab.id)))
            .resizable(true)
            .default_size(280.0)
            .size_range(160.0..=640.0)
            .frame(
                egui::Frame::new()
                    .fill(t.background)
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ui, |ui| {
                close_chart = !chart::chart(ui, tab.id, res, &result_kinds, spec);
            });
    }
    if close_chart {
        chart_spec = None;
    }
    ui.data_mut(|d| d.insert_temp(chart_id, chart_spec));

    // Pending changes are shown in place: edited values, deleted rows struck through.
    let overlay = app::editable_ref(&tab.kind, &tab.source).and_then(|ts| ts.overlay(res));
    let shown = overlay.as_ref().map_or(res, |o| &o.result);
    let view = tab.view();
    let pinned = tab.pinned_count();
    let range = tab.range().filter(|r| r.r1 < shown.rows.len());
    let no_cols = Vec::new();
    let no_incoming = Vec::new();
    let table_tab = matches!(tab.kind, TabKind::Table(_));
    let (cols, edit, order, bool_pick, row_offset, has_pk, incoming) =
        match app::editable_mut(&mut tab.kind, &mut tab.source) {
            Some(ts) => {
                // Rows are addressed by their key: every key column must be in the result.
                let pk = ts.pk_names();
                let has_pk = !pk.is_empty() && pk.iter().all(|p| res.columns.contains(p));
                (
                    &ts.cols,
                    ts.edit.as_mut(),
                    ts.order.clone(),
                    ts.bool_pick,
                    if table_tab { ts.page * page_size() } else { 0 },
                    has_pk,
                    &ts.incoming,
                )
            }
            None => (&no_cols, None, None, None, 0, false, &no_incoming),
        };
    let can_edit = has_pk && !s.cfg.read_only;
    let kinds = values::column_kinds(res, cols);
    let selected = tab
        .selected
        .filter(|(r, c)| *r < res.rows.len() && *c < res.columns.len());
    let editing = edit.is_some() || bool_pick.is_some();

    // Row detail on the right, for the selected row.
    let mut detail_event = None;
    if *cx.show_detail
        && let Some((row, _)) = selected
        && tab.detail_hidden != Some(row)
    {
        egui::Panel::right(egui::Id::new(("detail", tab.id)))
            .frame(
                egui::Frame::new()
                    .fill(t.sidebar)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .resizable(true)
            .default_size(320.0)
            .size_range(240.0..=640.0)
            .show(ui, |ui| {
                detail_event = detail::detail_panel(
                    ui,
                    &detail::DetailRow {
                        tab_id: tab.id,
                        res: shown,
                        cols,
                        kinds: &kinds,
                        row,
                        row_number: row_offset + row + 1,
                        can_edit,
                    },
                );
            });
    }

    let typing = ui.ctx().egui_wants_keyboard_input();
    let key_event = if !editing && !typing {
        selected.and_then(|sel| keyboard_event(ui, sel, shown, &view, can_edit))
    } else {
        None
    };
    let mut scroll_to = match key_event {
        Some(GridEvent::Select(row, _) | GridEvent::Extend(row, _)) => Some(row),
        _ => None,
    };
    if let Some(range) = &range {
        selection_stats(ui, tab.id, shown, &kinds, range);
    }

    // Ctrl+F: tables search the database (the toolbar box); editor results are searched here.
    let find_id = egui::Id::new(("result_find", tab.id));
    if !typing && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::F)) {
        if editable {
            ui.memory_mut(|m| m.request_focus(egui::Id::new(("table_search", tab.id))));
        } else {
            ui.data_mut(|d| {
                let st = d.get_temp_mut_or_default::<Option<find::FindState>>(find_id);
                st.get_or_insert_with(Default::default).focus = true;
            });
        }
    }
    let mut result_find: Option<find::FindState> = ui.data(|d| d.get_temp(find_id)).flatten();
    let found = result_find
        .as_ref()
        .map(|f| found_cells(ui.ctx(), tab.id, shown, &kinds, &view, &f.query));
    let mut found_current = None;
    if let (Some(st), Some(found)) = (&mut result_find, &found) {
        let order = found;
        match find::find_bar(ui, st, order.len()) {
            Some(find::FindAction::Close) => {
                result_find = None;
            }
            Some(find::FindAction::Goto) => {
                if let Some(&(r, c)) = order.get(st.current.min(order.len().saturating_sub(1))) {
                    tab.selected = Some((r, c));
                    tab.anchor = None;
                    scroll_to = Some(r);
                }
            }
            _ => {}
        }
        if let Some(st) = &result_find {
            found_current = order.get(st.current.min(order.len().saturating_sub(1))).copied();
        }
    }
    ui.data_mut(|d| d.insert_temp(find_id, result_find.clone()));
    let found_set: Option<HashSet<(usize, usize)>> = result_find
        .as_ref()
        .and(found.as_ref())
        .map(|order| order.iter().copied().collect());
    let selected = tab
        .selected
        .filter(|(r, c)| *r < res.rows.len() && *c < res.columns.len());
    let opts = GridOpts {
        can_edit,
        selected,
        scroll_to,
        order: &order,
        row_offset,
        bool_pick,
        kinds: &kinds,
        changed: overlay.as_ref().map(|o| &o.changed),
        deleted: overlay.as_ref().map(|o| &o.deleted),
        can_filter: editable,
        range: range.as_ref(),
        view: &view,
        pinned,
        found: found_set.as_ref(),
        found_current,
        incoming,
    };
    // Wide results scroll sideways instead of running under the detail panel. A pinned cell
    // double-clicked while scrolled away scrolls back so its editor shows.
    let home_id = egui::Id::new(("hscroll_home", tab.id));
    let mut area = egui::ScrollArea::horizontal()
        .id_salt(("hscroll", tab.id))
        .auto_shrink([false, false]);
    if ui.data_mut(|d| d.remove_temp::<bool>(home_id)).is_some() {
        area = area.horizontal_scroll_offset(0.0);
    }
    let mut frozen = Vec::new();
    let out = area.show(ui, |ui| grid(ui, tab.id, shown, cols, edit, &opts, &mut frozen));
    let mut event = out.inner.or(key_event);
    if let Some(e) = paint_frozen(ui, tab.id, &frozen, out.state.offset.x, out.inner_rect) {
        if matches!(e, GridEvent::StartEdit(..)) {
            ui.data_mut(|d| d.insert_temp(home_id, true));
        }
        event = Some(e);
    }
    if res.rows.is_empty() {
        ui.add_space(32.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("Nenhuma linha").color(t.subtle));
        });
    }

    if let (Some(a), Some((row, _))) = (detail_event, selected) {
        match a {
            DetailAction::Commit(ci, v) => event = Some(GridEvent::Commit(row, ci, v)),
            DetailAction::Copy(how) => event = Some(GridEvent::Copy(row, how)),
            // Hides it for this row only; clicking a cell brings it back. The toolbar toggle
            // is the persistent on/off.
            DetailAction::Close => tab.detail_hidden = Some(row),
        }
    }
    match event.and_then(|e| app::handle_grid_event(tab, s, e, can_edit)) {
        Some(Effect::Copy(text)) => ui.ctx().copy_text(text),
        Some(Effect::OpenFiltered {
            schema,
            table,
            filter,
        }) => {
            cx.request = Some(Request::OpenFiltered(schema, table, filter));
        }
        None => {}
    }
}

/// Pinned columns (and the row numbers) drawn again at the left edge once the grid is scrolled
/// sideways past them. Clicks on them select, double-clicks edit, like the cells underneath.
fn paint_frozen(
    ui: &mut egui::Ui,
    tab_id: u64,
    frozen: &[grid::Frozen],
    offset: f32,
    viewport: egui::Rect,
) -> Option<GridEvent> {
    if offset <= 0.0 || frozen.is_empty() {
        return None;
    }
    let t = theme::tokens();
    let mut event = None;
    let mut right_edge = f32::MIN;
    for f in frozen {
        let rect = f.rect.translate(egui::vec2(offset, 0.0));
        // Only the vertical part of the cell's clip counts (rows under the header); sideways,
        // its own column clip is exactly what scrolled out of view.
        let clip = egui::Rect::from_x_y_ranges(viewport.x_range(), f.clip.y_range()).intersect(viewport);
        if !clip.intersects(rect) {
            continue;
        }
        right_edge = right_edge.max(rect.right());
        let painter = ui.painter().with_clip_rect(clip);
        painter.rect_filled(rect, 0.0, f.bg);
        let stroke = egui::Stroke::new(1.0, t.border);
        painter.hline(rect.x_range(), rect.bottom() - 0.5, stroke);
        painter.vline(rect.right() - 0.5, rect.y_range(), stroke);
        let text_clip = painter.with_clip_rect(clip.intersect(rect.shrink2(egui::vec2(4.0, 0.0))));
        let (pos, align) = if f.right_align {
            (
                rect.right_center() - egui::vec2(8.0, 0.0),
                egui::Align2::RIGHT_CENTER,
            )
        } else if f.col.is_none() {
            (rect.center(), egui::Align2::CENTER_CENTER)
        } else {
            (
                rect.left_center() + egui::vec2(8.0, 0.0),
                egui::Align2::LEFT_CENTER,
            )
        };
        text_clip.text(pos, align, &f.text, f.font.clone(), f.color);
        if let (Some(row), Some(col)) = (f.row, f.col) {
            let r = ui.interact(
                rect.intersect(clip),
                egui::Id::new(("frozen", tab_id, row, col)),
                egui::Sense::click(),
            );
            if r.double_clicked() {
                event = Some(GridEvent::StartEdit(row, col));
            } else if r.clicked() {
                let shift = ui.input(|i| i.modifiers.shift);
                event = Some(if shift {
                    GridEvent::Extend(row, col)
                } else {
                    GridEvent::Select(row, col)
                });
            }
        }
    }
    // A soft edge where the pinned block ends, so it reads as floating over the rest.
    if right_edge > f32::MIN {
        let shadow = egui::Rect::from_x_y_ranges(right_edge..=right_edge + 6.0, viewport.y_range());
        ui.painter()
            .with_clip_rect(viewport)
            .rect_filled(shadow, 0.0, egui::Color32::from_black_alpha(40));
    }
    event
}

/// Cells whose shown text contains `query` (any case), in reading order. Cached: it runs every
/// frame while the find bar is open.
fn found_cells(
    ctx: &egui::Context,
    tab_id: u64,
    res: &QueryResult,
    kinds: &[values::ValueKind],
    view: &[usize],
    query: &str,
) -> Vec<(usize, usize)> {
    let key = egui::Id::new((query, res.rows.len(), res.elapsed, view));
    let cache = egui::Id::new(("result_find_cache", tab_id));
    if let Some((k, v)) = ctx.data(|d| d.get_temp::<(egui::Id, Vec<(usize, usize)>)>(cache))
        && k == key
    {
        return v;
    }
    let needle = query.to_lowercase();
    let mut out = Vec::new();
    if !needle.is_empty() {
        for (ri, row) in res.rows.iter().enumerate() {
            for &ci in view {
                if let Some(v) = &row[ci]
                    && grid::display_value(kinds[ci], v).to_lowercase().contains(&needle)
                {
                    out.push((ri, ci));
                }
            }
        }
    }
    ctx.data_mut(|d| d.insert_temp(cache, (key, out.clone())));
    out
}

/// "Definir valor nas N células": type the value (or choose NULL).
fn fill_dialog(ctx: &egui::Context, tab: &mut Tab) -> Option<GridEvent> {
    let cells = tab.selection().map_or(0, |s| (s.r1 - s.r0 + 1) * s.cols.len());
    let value = tab.fill_prompt.as_mut()?;
    let t = theme::tokens();
    let mut out = None;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::new(("fill", tab.id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(
                egui::RichText::new(format!("Valor para {cells} célula(s)"))
                    .size(15.0)
                    .color(t.foreground),
            );
            ui.label(
                egui::RichText::new("Fica pendente até salvar (Ctrl+S), como as outras edições.")
                    .size(11.0)
                    .color(t.subtle),
            );
            ui.add_space(8.0);
            let r = ui.add(
                TextEdit::singleline(value)
                    .font(egui::FontId::monospace(12.5))
                    .desired_width(f32::INFINITY),
            );
            r.request_focus();
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                out = Some(GridEvent::FillSelection(Some(value.clone())));
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(theme::primary_button(None, "Aplicar")).clicked() {
                    out = Some(GridEvent::FillSelection(Some(value.clone())));
                }
                if ui.add(theme::outline_button("NULL")).clicked() {
                    out = Some(GridEvent::FillSelection(None));
                }
                if ui.add(theme::outline_button("Cancelar")).clicked() {
                    cancel = true;
                }
            });
        });
    if out.is_none() && (cancel || modal.should_close()) {
        tab.fill_prompt = None;
    }
    out
}

/// "Estatísticas da coluna": fill rate, distinct values, range and the most common values, each
/// one a click away from becoming a filter (table tabs).
fn stats_dialog(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let Some(view) = &tab.col_stats else { return };
    let t = theme::tokens();
    let can_filter = matches!(tab.kind, TabKind::Table(_));
    let mut close = false;
    let mut filter: Option<Option<String>> = None;
    let modal = egui::Modal::new(egui::Id::new(("col_stats", tab.id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(460.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::CHART_BAR).size(16.0).color(t.lime));
                ui.label(egui::RichText::new(&view.column).size(15.0).color(t.foreground));
                ui.label(egui::RichText::new(&view.scope).size(11.0).color(t.subtle));
            });
            ui.add_space(8.0);
            match &view.data {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Contando…").color(t.muted_foreground));
                    });
                }
                Some(Err(e)) => {
                    ui.label(egui::RichText::new(e).color(t.destructive));
                }
                Some(Ok(st)) => {
                    let pct = |n: i64| {
                        if st.total > 0 {
                            format!("{:.1}%", n as f64 * 100.0 / st.total as f64).replace('.', ",")
                        } else {
                            "–".into()
                        }
                    };
                    egui::Grid::new(("col_stats_grid", tab.id))
                        .num_columns(2)
                        .spacing([16.0, 4.0])
                        .show(ui, |ui| {
                            let row = |ui: &mut egui::Ui, k: &str, v: String| {
                                ui.label(egui::RichText::new(k).color(t.muted_foreground));
                                ui.label(egui::RichText::new(v).monospace().color(t.foreground));
                                ui.end_row();
                            };
                            row(ui, "Linhas", palette::format_count(st.total));
                            row(
                                ui,
                                "Nulos",
                                format!("{} · {}", palette::format_count(st.nulls), pct(st.nulls)),
                            );
                            row(ui, "Distintos", palette::format_count(st.distinct));
                            if let Some(v) = &st.min {
                                row(ui, "Mínimo", v.clone());
                            }
                            if let Some(v) = &st.max {
                                row(ui, "Máximo", v.clone());
                            }
                        });
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new("VALORES MAIS COMUNS")
                            .size(11.0)
                            .color(t.subtle),
                    );
                    ui.add_space(4.0);
                    let most = st.top.first().map_or(1, |(_, n)| (*n).max(1));
                    for (v, n) in &st.top {
                        let r = ui
                            .horizontal(|ui| {
                                let (rect, _) =
                                    ui.allocate_exact_size(egui::vec2(220.0, 20.0), egui::Sense::hover());
                                // Bar behind the value, relative to the most common one.
                                let mut bar = rect;
                                bar.set_width(rect.width() * (*n as f32 / most as f32));
                                ui.painter()
                                    .rect_filled(bar, 0.0, theme::mix(t.lime, 0.14, t.background));
                                let (text, color) = match v {
                                    Some(v) => (v.clone(), t.foreground),
                                    None => ("NULL".into(), t.subtle),
                                };
                                let galley = ui.painter().layout(
                                    text,
                                    egui::FontId::monospace(12.0),
                                    color,
                                    rect.width() - 8.0,
                                );
                                let pos =
                                    egui::pos2(rect.left() + 4.0, rect.center().y - galley.size().y / 2.0);
                                ui.painter().with_clip_rect(rect).galley(pos, galley, color);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} · {}",
                                        palette::format_count(*n),
                                        pct(*n)
                                    ))
                                    .monospace()
                                    .size(12.0)
                                    .color(t.muted_foreground),
                                );
                                if can_filter
                                    && ui
                                        .add(
                                            egui::Button::new(
                                                theme::icon(icon::FUNNEL).color(t.muted_foreground),
                                            )
                                            .frame(false),
                                        )
                                        .on_hover_text("Filtrar a tabela por este valor")
                                        .clicked()
                                {
                                    filter = Some(v.clone());
                                }
                            })
                            .response;
                        if let Some(v) = v {
                            r.on_hover_text(v);
                        }
                    }
                    if st.top.is_empty() {
                        ui.label(egui::RichText::new("Nenhuma linha").color(t.subtle));
                    }
                }
            }
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(theme::outline_button("Fechar")).clicked() {
                    close = true;
                }
            });
        });
    let column = view.column.clone();
    if let Some(v) = filter {
        tab.col_stats = None;
        app::filter_by_value(tab, s, &column, v.as_deref());
    } else if close || modal.should_close() {
        tab.col_stats = None;
    }
}

/// Arrow keys move the selected cell; Enter or F2 starts editing it; Ctrl+C copies it.
fn keyboard_event(
    ui: &egui::Ui,
    (row, col): (usize, usize),
    res: &QueryResult,
    view: &[usize],
    can_edit: bool,
) -> Option<GridEvent> {
    let last_row = res.rows.len().checked_sub(1)?;
    // Left/right follow the on-screen order (pinned first, hidden skipped).
    let pos = view.iter().position(|&c| c == col).unwrap_or(0);
    let right = view.get(pos + 1).copied().unwrap_or(col);
    let left = pos
        .checked_sub(1)
        .and_then(|p| view.get(p))
        .copied()
        .unwrap_or(col);
    let clipboard = ui.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Copy => Some(GridEvent::CopyCells { headers: false }),
            egui::Event::Paste(text) if can_edit => Some(GridEvent::Paste(text.clone())),
            _ => None,
        })
    });
    if clipboard.is_some() {
        return clipboard;
    }
    ui.input(|i| {
        // Shift+arrows grow the selection instead of moving it.
        let to = |r, c| {
            if i.modifiers.shift {
                GridEvent::Extend(r, c)
            } else {
                GridEvent::Select(r, c)
            }
        };
        if i.modifiers.command && i.key_pressed(Key::A) {
            Some(GridEvent::SelectAll)
        } else if can_edit && i.modifiers.command && i.key_pressed(Key::D) {
            Some(GridEvent::FillDown)
        } else if can_edit && i.key_pressed(Key::Delete) {
            Some(GridEvent::DeleteRows)
        } else if can_edit && (i.key_pressed(Key::Enter) || i.key_pressed(Key::F2)) {
            Some(GridEvent::StartEdit(row, col))
        } else if i.key_pressed(Key::ArrowDown) {
            Some(to((row + 1).min(last_row), col))
        } else if i.key_pressed(Key::ArrowUp) {
            Some(to(row.saturating_sub(1), col))
        } else if i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::Tab) {
            Some(to(row, right))
        } else if i.key_pressed(Key::ArrowLeft) {
            Some(to(row, left))
        } else {
            None
        }
    })
}

/// "6 células · 4 valores · soma 5,5 · média 2,75 · mín 1,5 · máx 4" under a range selection.
fn selection_stats(
    ui: &mut egui::Ui,
    tab_id: u64,
    shown: &QueryResult,
    kinds: &[values::ValueKind],
    range: &values::CellRange,
) {
    let t = theme::tokens();
    let st = values::stats(shown, kinds, range);
    let mut parts = vec![format!("{} células", st.cells), format!("{} valores", st.values)];
    if let Some((n, sum, lo, hi)) = st.numbers {
        let f = values::format_number;
        parts.push(format!("soma {}", f(sum)));
        parts.push(format!("média {}", f(sum / n as f64)));
        parts.push(format!("mín {}", f(lo)));
        parts.push(format!("máx {}", f(hi)));
    }
    let text = parts.join("  ·  ");
    egui::Panel::bottom(egui::Id::new(("stats", tab_id)))
        .frame(
            egui::Frame::new()
                .fill(t.sidebar)
                .inner_margin(egui::Margin::symmetric(8, 4)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&text)
                        .size(12.0)
                        .monospace()
                        .color(t.muted_foreground),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(theme::icon(icon::COPY).color(t.muted_foreground)).frame(false),
                        )
                        .on_hover_text("Copiar resumo")
                        .clicked()
                    {
                        ui.ctx().copy_text(text.clone());
                    }
                });
            });
        });
}

/// Icon toggle for the row detail panel.
fn detail_toggle(ui: &mut egui::Ui, show: &mut bool) {
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

enum FilterAction {
    Apply(String),
    Save(String),
    Delete(String),
}

/// The funnel before WHERE: this table's saved filters, and saving the current one by name.
fn filters_menu(
    ui: &mut egui::Ui,
    tab_id: u64,
    all: &[SavedFilter],
    conn: &str,
    ts: &app::TableState,
) -> Option<FilterAction> {
    let t = theme::tokens();
    let mine = filters::for_table(all, conn, &ts.schema, &ts.table);
    let color = if mine.is_empty() {
        t.muted_foreground
    } else {
        t.lime
    };
    let mut action = None;
    let name_id = egui::Id::new(("filter_name", tab_id));
    ui.menu_button(theme::icon(icon::FUNNEL).size(15.0).color(color), |ui| {
        ui.set_width(340.0);
        ui.label(egui::RichText::new("FILTROS SALVOS").size(11.0).color(t.subtle));
        if mine.is_empty() {
            ui.label(egui::RichText::new("Nenhum filtro salvo para esta tabela").color(t.subtle));
        }
        for f in &mine {
            ui.horizontal(|ui| {
                let active = f.filter == ts.filter.trim();
                let r = ui
                    .add(
                        egui::Button::new(egui::RichText::new(&f.name).color(t.foreground))
                            .selected(active)
                            .frame_when_inactive(false),
                    )
                    .on_hover_text(egui::RichText::new(&f.filter).monospace());
                if r.clicked() {
                    action = Some(FilterAction::Apply(f.filter.clone()));
                    ui.close();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(theme::icon(icon::X).size(11.0).color(t.subtle)).frame(false))
                        .on_hover_text("Excluir filtro")
                        .clicked()
                    {
                        action = Some(FilterAction::Delete(f.id.clone()));
                    }
                });
            });
        }
        ui.separator();
        let can_save = !ts.filter.trim().is_empty();
        let mut name = ui.data(|d| d.get_temp::<String>(name_id)).unwrap_or_default();
        ui.horizontal(|ui| {
            let r = ui.add_enabled(
                can_save,
                TextEdit::singleline(&mut name)
                    .hint_text(if can_save {
                        "Nome do filtro atual"
                    } else {
                        "Escreva um WHERE para salvar"
                    })
                    .desired_width(220.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let ok = can_save && !name.trim().is_empty();
            if (ui.add_enabled(ok, egui::Button::new("Salvar")).clicked() || (enter && ok))
                && action.is_none()
            {
                action = Some(FilterAction::Save(name.trim().to_string()));
                name.clear();
                ui.close();
            }
        });
        ui.data_mut(|d| d.insert_temp(name_id, name));
    })
    .response
    .on_hover_text("Filtros salvos");
    action
}

enum FavAction {
    Load(String),
    Save,
    Delete(String),
}

/// "★ Favoritos" dropdown: save the current query, or load / delete a saved one.
fn favorites_menu(ui: &mut egui::Ui, favs: &[Favorite], conn: &str, can_save: bool) -> Option<FavAction> {
    let t = theme::tokens();
    let mut action = None;
    let label = theme::icon_text(icon::STAR, "Favoritos", t.warning, t.foreground);
    ui.menu_button(label, |ui| {
        ui.set_width(380.0);
        let save = theme::icon_text(icon::PLUS, "Salvar consulta atual…", t.lime, t.foreground);
        if ui
            .add_enabled(can_save, egui::Button::new(save).frame_when_inactive(false))
            .clicked()
        {
            action = Some(FavAction::Save);
            ui.close();
        }
        ui.separator();
        let mine = favorites::for_conn(favs, conn);
        if mine.is_empty() {
            ui.label(egui::RichText::new("Nenhuma consulta salva").color(t.subtle));
        }
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for f in mine {
                let scope = if f.conn.is_empty() {
                    "todas as conexões"
                } else {
                    ""
                };
                let r = ui.add(
                    egui::Button::new(egui::RichText::new(&f.name).color(t.foreground))
                        .frame_when_inactive(false)
                        .truncate()
                        .min_size(egui::vec2(ui.available_width(), 22.0))
                        .right_text(egui::RichText::new(scope).size(11.0).color(t.subtle)),
                );
                r.context_menu(|ui| {
                    if ui.button("Excluir").clicked() {
                        action = Some(FavAction::Delete(f.id.clone()));
                        ui.close();
                    }
                });
                if r.on_hover_text(&f.sql).clicked() {
                    action = Some(FavAction::Load(f.sql.clone()));
                    ui.close();
                }
            }
        });
        ui.label(
            egui::RichText::new("Clique direito num item para excluir")
                .size(10.0)
                .color(t.subtle),
        );
    });
    action
}

/// "Histórico" dropdown with this connection's recent queries; returns the one picked.
enum HistoryPick {
    Load(String),
    All,
}

fn history_menu(ui: &mut egui::Ui, history: &[Entry], conn: &str) -> Option<HistoryPick> {
    let t = theme::tokens();
    let mut picked = None;
    let label = theme::icon_text(
        icon::CLOCK_COUNTER_CLOCKWISE,
        "Histórico",
        t.muted_foreground,
        t.foreground,
    );
    ui.menu_button(label, |ui| {
        ui.set_width(460.0);
        let now = history::now();
        let mine: Vec<&Entry> = history.iter().rev().filter(|e| e.conn == conn).take(40).collect();
        let all = theme::icon_text(
            icon::LIST_MAGNIFYING_GLASS,
            "Ver histórico completo…",
            t.lime,
            t.foreground,
        );
        if ui
            .add(
                egui::Button::new(all)
                    .frame_when_inactive(false)
                    .shortcut_text("Ctrl+Shift+H"),
            )
            .clicked()
        {
            picked = Some(HistoryPick::All);
            ui.close();
        }
        ui.separator();
        if mine.is_empty() {
            ui.label(egui::RichText::new("Nenhuma consulta ainda").color(t.subtle));
        }
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for e in mine {
                let first = e.sql.trim().lines().next().unwrap_or_default();
                let more = if e.sql.trim().lines().count() > 1 {
                    " …"
                } else {
                    ""
                };
                let text = egui::RichText::new(format!("{first}{more}"))
                    .monospace()
                    .size(12.0)
                    .color(if e.ok { t.foreground } else { t.destructive });
                let r = ui.add(
                    egui::Button::new(text)
                        .frame_when_inactive(false)
                        .truncate()
                        .min_size(egui::vec2(ui.available_width(), 22.0))
                        .right_text(
                            egui::RichText::new(history::ago(e.at, now))
                                .size(11.0)
                                .color(t.subtle),
                        ),
                );
                if r.on_hover_text(&e.sql).clicked() {
                    picked = Some(HistoryPick::Load(e.sql.clone()));
                    ui.close();
                }
            }
        });
    });
    picked
}
