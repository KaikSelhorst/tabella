//! The tab strip and the body of each tab (SQL editor or table browser). Drawing only: edits,
//! queries and deletions are carried out by `crate::app`.

use eframe::egui::{self, Align, Key, Layout, TextEdit};

use super::detail::{self, DetailAction};
use super::grid::{GridOpts, grid};
use super::structure::{self, InsertAction, StructAction};
use super::{complete, icon, palette, syntax, theme};
use crate::app::{self, App, FavPrompt, GridEvent, Session, Tab, TabKind};
use crate::db::QueryResult;
use crate::export::{self, Format};
use crate::favorites::{self, Favorite};
use crate::history::{self, Entry};
use crate::sql::{self, PAGE_SIZE};
use crate::values;

/// What a tab body needs from the app besides the tab itself.
struct Cx<'a> {
    s: &'a Session,
    show_detail: &'a mut bool,
    history: &'a [Entry],
    favorites: &'a [Favorite],
    /// Something only the app can do (open another tab), carried out after drawing.
    request: Option<Request>,
}

enum Request {
    Structure(String, String),
    Data(String, String),
    Sql(String),
    SaveFavorite(String),
    DeleteFavorite(String),
}

impl App {
    pub fn central(&mut self, ui: &mut egui::Ui) {
        if self.tabs.is_empty() || !self.session.as_ref().is_some_and(|s| s.ready) {
            self.welcome(ui);
            return;
        }
        let t = theme::tokens();
        let mut close = None;
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
                        };
                        if theme::tab_label(ui, glyph, &tab.title, i == self.active).clicked() {
                            self.active = i;
                        }
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
        let (Some(s), Some(tab)) = (&self.session, self.tabs.get_mut(self.active)) else {
            return;
        };
        let mut cx = Cx {
            s,
            show_detail: &mut self.show_detail,
            history: &self.history,
            favorites: &self.favorites,
            request: None,
        };
        match &tab.kind {
            TabKind::Sql { .. } => sql_tab(ui, tab, &mut cx),
            TabKind::Table(_) => table_tab(ui, tab, &mut cx),
            TabKind::Structure(st) => {
                if let Some(e) = &tab.error {
                    error_banner(ui, e);
                }
                cx.request = match structure::structure_tab(ui, st, tab.busy()) {
                    Some(StructAction::OpenSql(ddl)) => Some(Request::Sql(ddl)),
                    Some(StructAction::OpenData) => Some(Request::Data(st.schema.clone(), st.table.clone())),
                    None => None,
                };
            }
        }
        match cx.request {
            Some(Request::Structure(schema, table)) => self.open_structure(&schema, &table),
            Some(Request::Data(schema, table)) => self.open_table(&schema, &table),
            Some(Request::Sql(text)) => self.open_sql(text),
            Some(Request::SaveFavorite(sql)) => {
                self.fav_prompt = Some(FavPrompt {
                    name: String::new(),
                    sql,
                    shared: false,
                });
            }
            Some(Request::DeleteFavorite(id)) => self.delete_favorite(&id),
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
    let TabKind::Sql { text } = &mut tab.kind else {
        return;
    };
    let mut run: Option<String> = None;
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
            let selected_range = TextEdit::load_state(ui.ctx(), editor_id)
                .and_then(|st| st.cursor.char_range())
                .map(|r| r.as_sorted_char_range())
                .map(|r| (usize::from(r.start), usize::from(r.end)))
                .filter(|(a, b)| a < b);
            let selection = selected_range.map(|(a, b)| text.chars().skip(a).take(b - a).collect::<String>());
            let format_key =
                ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT | egui::Modifiers::ALT, Key::F));
            ui.horizontal(|ui| {
                let label = if selection.is_some() {
                    "Executar seleção"
                } else {
                    "Executar"
                };
                if ui
                    .add_enabled(!busy, theme::primary_button(Some(icon::PLAY), label))
                    .on_hover_text("Ctrl+Enter ou F5")
                    .clicked()
                {
                    run = Some(selection.clone().unwrap_or_else(|| text.clone()));
                }
                if busy {
                    busy_indicator(ui, s);
                }
                if let Some(sql) = history_menu(ui, cx.history, &s.cfg.name) {
                    *text = sql;
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
                    ui.label(egui::RichText::new("Ctrl+Enter / F5").size(11.0).color(t.subtle));
                });
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let popup = complete::before_editor(ui.ctx(), editor_id, text, &s.tables, &s.columns);
                    let out = TextEdit::multiline(text)
                        .id(editor_id)
                        .code_editor()
                        .hint_text("SELECT * FROM …")
                        .desired_width(f32::INFINITY)
                        .min_size(ui.available_size())
                        .lock_focus(true)
                        .layouter(&mut syntax::sql_layouter(13.0))
                        .show(ui);
                    if let (Some((sugg, selected)), Some(range)) = (popup, out.cursor_range) {
                        let at = out.galley.pos_from_cursor(range.primary);
                        let anchor = out.galley_pos + at.left_bottom().to_vec2() + egui::vec2(0.0, 2.0);
                        complete::popup(ui.ctx(), editor_id, anchor, &sugg, selected);
                    }
                    let ctrl_enter = ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
                    if (out.response.has_focus() && ctrl_enter) || f5 {
                        run = Some(selection.clone().unwrap_or_else(|| text.clone()));
                    }
                });
        });
    if let Some(sql) = run.filter(|q| !q.trim().is_empty())
        && !busy
    {
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
    if let Some((format, _)) = export {
        app::start_export(tab, s, format, false);
    }
    confirm_run(ui.ctx(), tab, s);
    export_banner(ui, tab);
    result_area(ui, tab, cx, false);
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

fn table_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
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
    let full = rows >= PAGE_SIZE;
    let can_export = !busy && tab.result.as_ref().is_some_and(|r| !r.columns.is_empty());
    let mut export = None;
    let mut show_structure = false;
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
                    let first = ts.page * PAGE_SIZE + 1;
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
                ui.label(theme::icon(icon::FUNNEL).size(15.0).color(t.muted_foreground));
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
    let TabKind::Table(ts) = &mut tab.kind else { return };
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
    } else if save && let TabKind::Table(ts) = &mut tab.kind {
        ts.confirm_save = true;
    }
}

/// Summary before saving: what will change, and the exact SQL. Deletions make the confirm
/// button red; Enter never confirms here.
fn confirm_save(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
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
    let t = theme::tokens();
    if let Some(e) = &tab.error {
        error_banner(ui, e);
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
    if !editable {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(8, 2))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(summary(res))
                        .small()
                        .color(t.muted_foreground),
                );
            });
    }
    if res.columns.is_empty() {
        return;
    }

    // Pending changes are shown in place: edited values, deleted rows struck through.
    let overlay = match &tab.kind {
        TabKind::Table(ts) if editable => ts.overlay(res),
        _ => None,
    };
    let shown = overlay.as_ref().map_or(res, |o| &o.result);
    let no_cols = Vec::new();
    let (cols, edit, order, bool_pick, row_offset, has_pk) = match &mut tab.kind {
        TabKind::Table(ts) if editable => {
            let has_pk = ts.cols.iter().any(|c| c.pk);
            (
                &ts.cols,
                ts.edit.as_mut(),
                ts.order.clone(),
                ts.bool_pick,
                ts.page * PAGE_SIZE,
                has_pk,
            )
        }
        _ => (&no_cols, None, None, None, 0, false),
    };
    let can_edit = editable && has_pk && !s.cfg.read_only;
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

    let key_event = if !editing && !ui.ctx().egui_wants_keyboard_input() {
        selected.and_then(|sel| keyboard_event(ui, sel, shown, can_edit))
    } else {
        None
    };
    let scroll_to = match key_event {
        Some(GridEvent::Select(row, _)) => Some(row),
        _ => None,
    };
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
    };
    // Wide results scroll sideways instead of running under the detail panel.
    let mut event = egui::ScrollArea::horizontal()
        .id_salt(("hscroll", tab.id))
        .auto_shrink([false, false])
        .show(ui, |ui| grid(ui, tab.id, shown, cols, edit, &opts))
        .inner
        .or(key_event);
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
    if let Some(event) = event
        && let Some(text) = app::handle_grid_event(tab, s, event, can_edit)
    {
        ui.ctx().copy_text(text);
    }
}

/// Arrow keys move the selected cell; Enter or F2 starts editing it; Ctrl+C copies it.
fn keyboard_event(
    ui: &egui::Ui,
    (row, col): (usize, usize),
    res: &QueryResult,
    can_edit: bool,
) -> Option<GridEvent> {
    let (last_row, last_col) = (res.rows.len().checked_sub(1)?, res.columns.len().checked_sub(1)?);
    if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
        ui.ctx().copy_text(res.rows[row][col].clone().unwrap_or_default());
        return None;
    }
    ui.input(|i| {
        if can_edit && (i.key_pressed(Key::Enter) || i.key_pressed(Key::F2)) {
            Some(GridEvent::StartEdit(row, col))
        } else if i.key_pressed(Key::ArrowDown) {
            Some(GridEvent::Select((row + 1).min(last_row), col))
        } else if i.key_pressed(Key::ArrowUp) {
            Some(GridEvent::Select(row.saturating_sub(1), col))
        } else if i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::Tab) {
            Some(GridEvent::Select(row, (col + 1).min(last_col)))
        } else if i.key_pressed(Key::ArrowLeft) {
            Some(GridEvent::Select(row, col.saturating_sub(1)))
        } else {
            None
        }
    })
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
fn history_menu(ui: &mut egui::Ui, history: &[Entry], conn: &str) -> Option<String> {
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
                    picked = Some(e.sql.clone());
                    ui.close();
                }
            }
        });
    });
    picked
}
