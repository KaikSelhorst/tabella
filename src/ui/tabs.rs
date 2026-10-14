//! The tab strip and the body of each tab (SQL editor or table browser). Drawing only: edits,
//! queries and deletions are carried out by `crate::app`.

use eframe::egui::{self, Align, Key, Layout, TextEdit};

use super::detail::{self, DetailAction};
use super::grid::{GridOpts, grid};
use super::{complete, icon, palette, syntax, theme};
use crate::app::{self, App, GridEvent, Session, Tab, TabKind};
use crate::db::QueryResult;
use crate::history::{self, Entry};
use crate::sql::{self, PAGE_SIZE};
use crate::values;

/// What a tab body needs from the app besides the tab itself.
struct Cx<'a> {
    s: &'a Session,
    show_detail: &'a mut bool,
    history: &'a [Entry],
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
        };
        match &tab.kind {
            TabKind::Sql { .. } => sql_tab(ui, tab, &mut cx),
            TabKind::Table(_) => table_tab(ui, tab, &mut cx),
        }
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
            let selection = TextEdit::load_state(ui.ctx(), editor_id)
                .and_then(|st| st.cursor.char_range())
                .map(|r| r.as_sorted_char_range())
                .map(|r| (usize::from(r.start), usize::from(r.end)))
                .filter(|(a, b)| a < b)
                .map(|(a, b)| text.chars().skip(a).take(b - a).collect::<String>());
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
    confirm_run(ui.ctx(), tab, s);
    result_area(ui, tab, cx, false);
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
    let mut reload_now = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F5));
    let mut add_record = false;
    let status = status_text(tab, s.cfg.read_only);
    let busy = tab.busy();
    let rows = tab.result.as_ref().map_or(0, |r| r.rows.len());
    let full = rows >= PAGE_SIZE;
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
                // Measured after the labels; leaves room for the buttons on the right.
                let w = (ui.available_width() - 200.0).max(120.0);
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
                });
            })
        });
    let r = toolbar.response.rect;
    ui.painter()
        .hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, t.border));
    if add_record {
        app::insert_default_row(tab, s);
    } else if reload_now {
        app::reload(tab, s);
    }
    result_area(ui, tab, cx, true);
    confirm_delete(ui.ctx(), tab, s);
}

/// Asks before deleting; shows the exact WHERE clause that will run.
fn confirm_delete(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let (Some(res), TabKind::Table(ts)) = (&tab.result, &mut tab.kind) else {
        return;
    };
    let Some(row) = ts.confirm_delete.filter(|r| *r < res.rows.len()) else {
        ts.confirm_delete = None;
        return;
    };
    let t = theme::tokens();
    let k = s.cfg.kind;
    let wh = sql::pk_where(k, &ts.pk_names(), &res.columns, &res.rows[row]);
    let (mut confirm, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new(("confirm_delete", tab.id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(420.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::TRASH).size(16.0).color(t.destructive));
                ui.label(
                    egui::RichText::new("Excluir 1 linha?")
                        .size(15.0)
                        .color(t.foreground),
                );
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("de {}.{} onde", ts.schema, ts.table)).color(t.muted_foreground),
            );
            ui.add_space(4.0);
            egui::Frame::new()
                .fill(t.sidebar)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(egui::RichText::new(wh.render(k)).monospace().size(12.0)).wrap());
                });
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                confirm = ui.add(danger_button("Excluir")).clicked();
                cancel = ui.add(theme::outline_button("Cancelar")).clicked();
            });
        });
    if confirm {
        app::confirm_delete(tab, s);
    } else if cancel || modal.should_close() {
        ts.confirm_delete = None;
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
                " · duplo clique ou Enter edita"
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
        egui::Frame::new()
            .fill(theme::mix(t.destructive, 0.12, t.background))
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(egui::RichText::new(e).color(t.destructive)).wrap());
            });
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
                        res,
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
        selected.and_then(|sel| keyboard_event(ui, sel, res, can_edit))
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
    };
    // Wide results scroll sideways instead of running under the detail panel.
    let mut event = egui::ScrollArea::horizontal()
        .id_salt(("hscroll", tab.id))
        .auto_shrink([false, false])
        .show(ui, |ui| grid(ui, tab.id, res, cols, edit, &opts))
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
