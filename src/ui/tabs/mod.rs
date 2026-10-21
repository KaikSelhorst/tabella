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
use crate::app::{self, App, Dialog, Effect, FavPrompt, GridEvent, RunMode, Session, Tab, TabKind};
use crate::db::{ActivityView, QueryResult};
use crate::definition;
use crate::export::{self, Format};
use crate::favorites::{self, Favorite};
use crate::filters::{self, SavedFilter};
use crate::history::{self, Entry};
use crate::sql::{self, page_size};
use crate::values;

mod activity;
mod compare;
mod dump;
mod editor;
mod query_history;
mod result_diff;
mod results;
mod script;
mod search;
mod table;

use activity::activity_tab;
use compare::compare_tab;
use dump::dump_tab;
use editor::{confirm_run, preview_rows, sql_tab};
use query_history::history_tab;
use results::{fk_dialog, result_area, status_text};
use script::script_tab;
use search::search_tab;
use table::{changes_bar, confirm_save, table_tab};

/// What a tab body needs from the app besides the tab itself.
pub(super) struct Cx<'a> {
    s: &'a Session,
    show_detail: &'a mut bool,
    history: &'a [Entry],
    favorites: &'a [Favorite],
    filters: &'a [SavedFilter],
    saved: &'a [crate::db::ConnConfig],
    /// Something only the app can do (open another tab), carried out after drawing.
    request: Option<Request>,
}

fn diagram_view(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let busy = tab.busy();
    let TabKind::Diagram(d) = &mut tab.kind else {
        return;
    };
    match diagram::diagram_tab(ui, tab.id, d, busy, s) {
        Some(DiagramAction::OpenTable(table)) => cx.request = Some(Request::Data(d.schema.clone(), table)),
        Some(DiagramAction::Schema(schema)) => cx.request = Some(Request::Diagram(schema)),
        None => {}
    }
}

fn design_view(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let busy = tab.busy();
    if let Some(e) = &tab.error {
        error_banner(ui, e);
    }
    let TabKind::Design(d) = &mut tab.kind else {
        return;
    };
    match design::design_tab(ui, tab.id, d, busy) {
        Some(DesignAction::Run(sql)) => {
            if s.cfg.read_only {
                tab.error = Some("Read-only connection: the structure can't be changed.".into());
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

/// From the tab strip's context menu.
pub(super) enum TabAction {
    Rename(usize, String),
    Duplicate(usize),
    /// Close the others (`true`: only those to the right).
    CloseOthers(usize, bool),
    /// Pin the tab at the start of its strip, or let go of it.
    TogglePin(usize),
    Reopen,
    /// Split right: the tab goes to the other pane (a SQL tab is copied there).
    Split(usize),
    CloseSplit,
}

/// The icon of a tab's kind (a view opens like a table, with the sidebar's eye).
fn tab_glyph(tab: &Tab, is_view: impl Fn(&str, &str) -> bool) -> &'static str {
    match &tab.kind {
        // A view opens like a table; its tab shows the sidebar's eye.
        TabKind::Table(ts) if is_view(&ts.schema, &ts.table) => icon::EYE,
        TabKind::Table(_) => icon::TABLE,
        TabKind::Sql(_) => icon::CODE,
        TabKind::Structure(_) => icon::TREE_STRUCTURE,
        TabKind::Activity(_) => icon::PULSE,
        TabKind::Design(_) => icon::PENCIL_SIMPLE,
        TabKind::Search(_) => icon::MAGNIFYING_GLASS,
        TabKind::History(_) => icon::CLOCK_COUNTER_CLOCKWISE,
        TabKind::Diagram(_) => icon::GRAPH,
        TabKind::Script(_) => icon::PLAY,
        TabKind::Dump(_) => icon::DOWNLOAD_SIMPLE,
        TabKind::Compare(_) => icon::GIT_DIFF,
    }
}

/// What the tab's context menu needs to know about the others.
struct TabMenu {
    index: usize,
    tabs: usize,
    closed: bool,
    split_on: bool,
}

/// A tab's context menu: rename, pin, duplicate, close others, split, reopen.
fn tab_menu(ui: &mut egui::Ui, tab: &Tab, m: &TabMenu) -> Option<TabAction> {
    let t = theme::tokens();
    let (i, n, closed, split_on) = (m.index, m.tabs, m.closed, m.split_on);
    let is_sql = matches!(tab.kind, TabKind::Sql(_));
    let can_split = is_sql || n > 1;
    let mut return_action = None;
    // Rename in place: type and press Enter.
    let name_id = egui::Id::new(("tab_name", tab.id));
    let mut name = ui
        .data(|d| d.get_temp::<String>(name_id))
        .unwrap_or_else(|| tab.title.clone());
    ui.label(egui::RichText::new("Tab name").size(11.0).color(t.subtle));
    let r = ui.add(TextEdit::singleline(&mut name).desired_width(200.0));
    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !name.trim().is_empty() {
        return_action = Some(TabAction::Rename(i, name.trim().to_string()));
        ui.data_mut(|d| d.remove::<String>(name_id));
        ui.close();
    } else {
        ui.data_mut(|d| d.insert_temp(name_id, name));
    }
    ui.separator();
    let pin_label = if tab.pinned { "Unpin tab" } else { "Pin tab" };
    if ui.button(pin_label).clicked() {
        return_action = Some(TabAction::TogglePin(i));
        ui.close();
    }
    if is_sql && ui.button("Duplicate").clicked() {
        return_action = Some(TabAction::Duplicate(i));
        ui.close();
    }
    if ui.add_enabled(n > 1, egui::Button::new("Close others")).clicked() {
        return_action = Some(TabAction::CloseOthers(i, false));
        ui.close();
    }
    if ui
        .add_enabled(i + 1 < n, egui::Button::new("Close tabs to the right"))
        .clicked()
    {
        return_action = Some(TabAction::CloseOthers(i, true));
        ui.close();
    }
    let split_label = if split_on {
        "Move to the other pane"
    } else {
        "Split right"
    };
    if ui
        .add_enabled(
            split_on || can_split,
            egui::Button::new(split_label).shortcut_text("Ctrl+\\"),
        )
        .clicked()
    {
        return_action = Some(TabAction::Split(i));
        ui.close();
    }
    if split_on && ui.button("Close split view").clicked() {
        return_action = Some(TabAction::CloseSplit);
        ui.close();
    }
    if ui
        .add_enabled(
            closed,
            egui::Button::new("Reopen closed tab").shortcut_text("Ctrl+Shift+T"),
        )
        .clicked()
    {
        return_action = Some(TabAction::Reopen);
        ui.close();
    }
    return_action
}

/// The list at the end of an overflowing strip: every tab of the pane, to pick one.
fn all_tabs_menu(ui: &mut egui::Ui, tabs: &[Tab], side: Option<bool>, sel: Option<usize>) -> Option<usize> {
    let t = theme::tokens();
    let mut pick = None;
    let list = theme::icon(icon::CARET_DOWN).color(t.muted_foreground);
    ui.menu_button(list, |ui| {
        egui::ScrollArea::vertical().max_height(480.0).show(ui, |ui| {
            for (i, tab) in tabs
                .iter()
                .enumerate()
                .filter(|(_, t)| side.is_none_or(|r| t.right == r))
            {
                if ui.selectable_label(Some(i) == sel, &tab.title).clicked() {
                    pick = Some(i);
                    ui.close();
                }
            }
        });
    })
    .response
    .on_hover_text(format!(
        "All {} tabs",
        tabs.iter().filter(|t| side.is_none_or(|r| t.right == r)).count()
    ));
    pick
}

pub(super) enum Request {
    Structure(String, String),
    Data(String, String),
    Sql(String),
    OpenFiltered(String, String, String),
    /// A bulk UPDATE to look at: opens it in an editor and previews what it would change.
    PreviewUpdate(String),
    SaveFavorite(String),
    DeleteFavorite(String),
    /// The ER diagram of a schema.
    Diagram(String),
    /// A view's definition in an editor.
    ViewSource(String, String),
    /// The code of a function, procedure, trigger, view or sequence in an editor.
    Object(crate::db::DbObject),
    /// Save the active editor tab to its file (`true`: ask where).
    SaveFile(bool),
    OpenFile,
    RunFile,
    History,
    /// Remove one history entry: (connection, sql).
    ForgetHistory(String, String),
    /// The structure of schema.table, pointing at a column.
    StructureColumn(String, String, String),
    /// "Edit structure" of schema.table (its Structure tab has the data).
    Design(String, String),
    /// schema, table, name, filter
    SaveFilter(String, String, String, String),
    DeleteFilter(String),
    /// Column widths to remember: schema, table, (column, width).
    ColumnWidths(String, String, Vec<(String, f32)>),
}

impl App {
    pub fn central(&mut self, ui: &mut egui::Ui) {
        if self.tabs.is_empty() || !self.session.as_ref().is_some_and(|s| s.ready) {
            self.welcome(ui);
            return;
        }
        self.sync_split();
        let request = if self.split.is_some() {
            self.split_panes(ui)
        } else {
            if !self.tab_strip(ui, None) {
                return;
            }
            self.sync_split();
            let (Some(s), Some(tab)) = (&self.session, self.tabs.get_mut(self.active)) else {
                return;
            };
            let body = ui.available_rect_before_wrap();
            let mut cx = Cx {
                s,
                show_detail: &mut self.show_detail,
                history: &self.library.history,
                favorites: &self.library.favorites,
                filters: &self.library.filters,
                saved: &self.connections.saved,
                request: None,
            };
            tab_body(ui, tab, &mut cx);
            let request = cx.request;
            self.edge_drop(ui, body);
            request
        };
        self.handle_request(request);
    }

    /// A tab dragged onto the right edge of the content opens the split view with it there.
    fn edge_drop(&mut self, ui: &mut egui::Ui, body: egui::Rect) {
        let ctx = ui.ctx().clone();
        let Some(id) = egui::DragAndDrop::payload::<u64>(&ctx).map(|p| *p) else {
            return;
        };
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        // Alone in the strip, only an editor can go (as a copy); there is nothing else to leave.
        if self.tabs.len() < 2 && self.tabs[i].editor().is_none() {
            return;
        }
        let zone = egui::Rect::from_min_max(
            egui::pos2(body.right() - (body.width() * 0.25).max(120.0), body.top()),
            body.right_bottom(),
        );
        if !ctx.pointer_interact_pos().is_some_and(|p| zone.contains(p)) {
            return;
        }
        let t = theme::tokens();
        let half = egui::Rect::from_min_max(egui::pos2(body.center().x, body.top()), body.right_bottom());
        ui.painter().rect_filled(half, 0.0, t.lime.gamma_multiply(0.12));
        ui.painter().rect_stroke(
            half,
            0.0,
            egui::Stroke::new(2.0, t.lime),
            egui::StrokeKind::Inside,
        );
        if ctx.input(|i| i.pointer.any_released()) {
            egui::DragAndDrop::clear_payload(&ctx);
            self.drag_to_new_pane(i);
        }
    }

    /// One pane's tab strip (`None`: the only one) and what was chosen in it. `false`: a tab was
    /// closed, so the rest of the frame is skipped.
    fn tab_strip(&mut self, ui: &mut egui::Ui, side: Option<bool>) -> bool {
        let t = theme::tokens();
        let mut close = None;
        let mut action: Option<TabAction> = None;
        let mut dropped: Option<u64> = None;
        // Where each tab of this strip is drawn, to tell where a dragged tab would go.
        let mut rects: Vec<(u64, egui::Rect)> = Vec::new();
        let tag = side.map_or(2u8, u8::from);
        // The tab this strip shows as selected, and whether the pane has the focus.
        let sel = match side {
            None => Some(self.active),
            Some(r) => self
                .split
                .as_ref()
                .and_then(|sp| self.tabs.iter().position(|t| t.id == sp.shown[usize::from(r)])),
        };
        let focused = self
            .tabs
            .get(self.active)
            .is_some_and(|t| side.is_none_or(|r| t.right == r));
        let strip = egui::Frame::new()
            .fill(t.sidebar)
            .inner_margin(egui::Margin::symmetric(8, 0))
            .show(ui, |ui| {
                ui.set_height(32.0);
                // The whole width of the pane is the drop zone for a dragged tab.
                ui.set_width(ui.available_width());
                // More tabs than fit: the strip scrolls (the wheel too), follows the active tab,
                // and a list at the end reaches any of them.
                let overflow_id = egui::Id::new(("tab_strip_overflow", tag));
                let overflowed = ui.data(|d| d.get_temp::<bool>(overflow_id)).unwrap_or(false);
                let follow_id = egui::Id::new(("tab_strip_active", tag));
                let active_id = sel.and_then(|i| self.tabs.get(i)).map(|t| t.id);
                let follow = ui.data(|d| d.get_temp::<Option<u64>>(follow_id)) != Some(active_id);
                ui.data_mut(|d| d.insert_temp(follow_id, active_id));
                let mut pick: Option<usize> = None;
                ui.horizontal_centered(|ui| {
                    let list_width = if overflowed { 32.0 } else { 0.0 };
                    let scroll = egui::ScrollArea::horizontal()
                        .id_salt(("tab_strip", tag))
                        .max_width(ui.available_width() - list_width)
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            ui.style_mut().always_scroll_the_only_direction = true;
                            ui.horizontal_centered(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                let is_view = |schema: &str, table: &str| {
                                    self.session.as_ref().is_some_and(|s| {
                                        s.tables
                                            .iter()
                                            .any(|t| t.is_view && t.schema == schema && t.name == table)
                                    })
                                };
                                for (i, tab) in self
                                    .tabs
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, t)| side.is_none_or(|r| t.right == r))
                                {
                                    let glyph = tab_glyph(tab, is_view);
                                    let shown = if app::file_dirty(tab) {
                                        format!("{} •", tab.title)
                                    } else {
                                        tab.title.clone()
                                    };
                                    let label = theme::tab_label(ui, glyph, &shown, Some(i) == sel);
                                    let label_rect = label.rect;
                                    rects.push((tab.id, label_rect));
                                    if label.clicked() {
                                        self.active = i;
                                    }
                                    if label.drag_started() {
                                        label.dnd_set_drag_payload(tab.id);
                                    }
                                    if label.dragged() {
                                        theme::tab_drag_ghost(ui, &label, glyph, &shown);
                                    }
                                    if follow && Some(i) == sel {
                                        label.scroll_to_me(None);
                                    }
                                    let menu = TabMenu {
                                        index: i,
                                        tabs: self.tabs.len(),
                                        closed: !self.closed.is_empty(),
                                        split_on: self.split.is_some(),
                                    };
                                    label.context_menu(|ui| {
                                        if let Some(a) = tab_menu(ui, tab, &menu) {
                                            action = Some(a);
                                        }
                                    });
                                    // A pinned tab has a pin where the close button would be; clicking
                                    // it lets go.
                                    let (glyph, tip) = if tab.pinned {
                                        (icon::PUSH_PIN, "Pinned: click to unpin")
                                    } else {
                                        (icon::X, "Close")
                                    };
                                    let x = theme::icon(glyph).size(11.0).color(if tab.pinned {
                                        t.lime
                                    } else {
                                        t.subtle
                                    });
                                    let x = ui.add(egui::Button::new(x).frame(false)).on_hover_text(tip);
                                    if Some(i) == sel {
                                        theme::tab_underline(ui, label_rect.union(x.rect), focused);
                                    }
                                    if x.clicked() {
                                        if tab.pinned {
                                            action = Some(TabAction::TogglePin(i));
                                        } else {
                                            close = Some(i);
                                        }
                                    }
                                    ui.add_space(12.0);
                                }
                            });
                        });
                    let hidden = scroll.content_size.x > scroll.inner_rect.width() + 1.0;
                    ui.data_mut(|d| d.insert_temp(overflow_id, hidden));
                    // The list button takes room: follow the active tab again next frame.
                    if hidden != overflowed {
                        ui.data_mut(|d| d.remove::<Option<u64>>(follow_id));
                        ui.ctx().request_repaint();
                    }
                    if hidden {
                        pick = all_tabs_menu(ui, &self.tabs, side, sel);
                    }
                });
                if let Some(i) = pick {
                    self.active = i;
                }
            });
        let r = strip.response.rect;
        // A dragged tab lands before the first tab whose middle is right of the pointer.
        let before = |x: f32| rects.iter().find(|(_, r)| x < r.center().x).map(|(id, _)| *id);
        let pointer_x = ui.input(|i| i.pointer.interact_pos()).map(|p| p.x);
        if let (Some(id), Some(x)) = (strip.response.dnd_hover_payload::<u64>(), pointer_x) {
            let from_here = self
                .tabs
                .iter()
                .any(|t| t.id == *id && side.is_none_or(|r| t.right == r));
            if from_here {
                // Reordering: a line where it would land.
                let at = match before(x) {
                    Some(b) => rects.iter().find(|(i, _)| *i == b).map(|(_, r)| r.left() - 6.0),
                    None => rects.last().map(|(_, r)| r.right() + 6.0),
                };
                if let Some(at) = at {
                    ui.painter().vline(
                        at,
                        r.shrink2(egui::vec2(0.0, 6.0)).y_range(),
                        egui::Stroke::new(2.0, t.lime),
                    );
                }
            } else {
                ui.painter()
                    .rect_stroke(r, 0.0, egui::Stroke::new(2.0, t.lime), egui::StrokeKind::Inside);
            }
        }
        if let Some(id) = strip.response.dnd_release_payload::<u64>() {
            dropped = Some(*id);
        }
        ui.painter()
            .hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, t.border));
        if let Some(i) = close {
            self.close_tab(i);
            return false;
        }
        if let Some(id) = dropped {
            let before = pointer_x.and_then(before);
            self.reorder_tab(id, before, side);
        }
        if let Some(a) = action {
            self.apply_tab_action(a);
        }
        true
    }

    /// Carries out what a tab's menu or pin asked for.
    fn apply_tab_action(&mut self, action: TabAction) {
        match action {
            TabAction::Rename(i, name) => {
                if let Some(tab) = self.tabs.get_mut(i) {
                    tab.title = name;
                    tab.renamed = true;
                }
            }
            TabAction::Duplicate(i) => self.duplicate_tab(i),
            TabAction::CloseOthers(i, right) => self.close_others(i, right),
            TabAction::TogglePin(i) => {
                if let Some(pinned) = self.tabs.get(i).map(|t| t.pinned) {
                    self.set_pinned(i, !pinned);
                }
            }
            TabAction::Reopen => self.reopen_closed_tab(),
            TabAction::Split(i) => self.split_tab(i),
            TabAction::CloseSplit => self.close_split(),
        }
    }

    /// Both panes of the split view, each with its own tab strip; returns the request of the one
    /// with focus. A request from the other pane moves focus there first, so it acts on the tab
    /// it came from.
    fn split_panes(&mut self, ui: &mut egui::Ui) -> Option<Request> {
        let t = theme::tokens();
        let mut from_focused = None;
        let mut from_other = None;
        // The unfocused pane's tab, when it was clicked or asked for something.
        let mut take_focus: Option<u64> = None;
        let mut abort = false;
        let mut divider = None;
        ui.columns(2, |cols| {
            for (col, ui) in cols.iter_mut().enumerate() {
                let side = col == 1;
                // A closed tab shifts the indices: draw nothing else this frame.
                if !self.tab_strip(ui, Some(side)) {
                    abort = true;
                    break;
                }
                self.sync_split();
                let shown = self.split.as_ref().map(|sp| sp.shown[col]);
                let idx = shown.and_then(|id| self.tabs.iter().position(|t| t.id == id));
                let (Some(idx), Some(s)) = (idx, self.session.as_ref()) else {
                    abort = true;
                    break;
                };
                let focused = self.tabs.get(self.active).is_some_and(|t| t.right == side);
                let pane = ui.max_rect();
                if side {
                    divider = Some(pane.left() - ui.spacing().item_spacing.x / 2.0);
                }
                let tab = &mut self.tabs[idx];
                let id = tab.id;
                ui.push_id(("pane", side), |ui| {
                    let mut cx = Cx {
                        s,
                        show_detail: &mut self.show_detail,
                        history: &self.library.history,
                        favorites: &self.library.favorites,
                        filters: &self.library.filters,
                        saved: &self.connections.saved,
                        request: None,
                    };
                    tab_body(ui, tab, &mut cx);
                    if focused {
                        from_focused = cx.request;
                    } else {
                        from_other = cx.request;
                    }
                });
                if !focused
                    && (from_other.is_some()
                        || (ui.input(|i| i.pointer.any_pressed()) && ui.rect_contains_pointer(pane)))
                {
                    take_focus = Some(id);
                }
            }
        });
        if abort {
            return None;
        }
        if let Some(x) = divider {
            let r = ui.min_rect();
            ui.painter()
                .vline(x, r.y_range(), egui::Stroke::new(1.0, t.border));
        }
        // The focused pane's request first: it was drawn while its tab was still the active one.
        self.handle_request(from_focused);
        if let Some(id) = take_focus
            && let Some(i) = self.tabs.iter().position(|t| t.id == id)
        {
            self.active = i;
            ui.ctx().request_repaint();
        }
        from_other
    }

    fn handle_request(&mut self, request: Option<Request>) {
        match request {
            Some(Request::Structure(schema, table)) => self.open_structure(&schema, &table),
            Some(Request::StructureColumn(schema, table, column)) => {
                self.open_structure_at(&schema, &table, Some(&column));
            }
            Some(Request::Data(schema, table)) => self.open_table(&schema, &table),
            Some(Request::OpenFiltered(schema, table, filter)) => {
                self.open_table_filtered(&schema, &table, filter)
            }
            Some(Request::Sql(text)) => self.open_sql(text),
            Some(Request::PreviewUpdate(sql)) => self.open_update_preview(sql),
            Some(Request::SaveFavorite(sql)) => {
                self.dialog = Some(Dialog::Favorite(FavPrompt {
                    name: String::new(),
                    sql,
                    shared: false,
                }));
            }
            Some(Request::DeleteFavorite(id)) => self.delete_favorite(&id),
            Some(Request::Diagram(schema)) => self.open_diagram(Some(&schema)),
            Some(Request::Object(object)) => self.open_object(object),
            Some(Request::ViewSource(schema, name)) => {
                let view = self.session.as_ref().and_then(|s| {
                    s.objects
                        .iter()
                        .find(|o| o.kind == crate::db::ObjKind::View && o.schema == schema && o.name == name)
                        .cloned()
                });
                match view {
                    Some(o) => self.open_object(o),
                    None => {
                        self.notice =
                            app::Notice::error(format!("Couldn't find the definition of view {name}"))
                    }
                }
            }
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
            Some(Request::ColumnWidths(schema, table, widths)) => {
                self.remember_widths(&schema, &table, &widths)
            }
            None => {}
        }
    }
}

/// Draws the body of one tab; what it asks of the app ends up in `cx.request`.
fn tab_body(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    match &tab.kind {
        TabKind::Sql(_) => sql_tab(ui, tab, cx),
        TabKind::Table(_) => table_tab(ui, tab, cx),
        TabKind::Activity(_) => activity_tab(ui, tab, cx),
        TabKind::Search(_) => search_tab(ui, tab, cx),
        TabKind::History(_) => history_tab(ui, tab, cx),
        TabKind::Script(_) => script_tab(ui, tab, cx),
        TabKind::Dump(_) => dump_tab(ui, tab, cx),
        TabKind::Compare(_) => compare_tab(ui, tab, cx),
        TabKind::Diagram(_) => diagram_view(ui, tab, cx),
        TabKind::Design(_) => design_view(ui, tab, cx),
        TabKind::Structure(st) => {
            if let Some(e) = &tab.error {
                error_banner(ui, e);
            }
            let view = s.is_view(&st.schema, &st.table);
            cx.request = match structure::structure_tab(ui, st, tab.id, tab.busy(), view) {
                Some(StructAction::OpenSql(ddl)) => Some(Request::Sql(ddl)),
                Some(StructAction::ViewSource) => {
                    Some(Request::ViewSource(st.schema.clone(), st.table.clone()))
                }
                Some(StructAction::Edit) => Some(Request::Design(st.schema.clone(), st.table.clone())),
                Some(StructAction::OpenData) => Some(Request::Data(st.schema.clone(), st.table.clone())),
                None => None,
            };
        }
    }
}

/// Removes what the tab views keep in egui's memory for tab `id`; each entry is replaced by
/// the next value while the tab lives, so only a closed tab's are ever left behind.
pub(crate) fn forget_tab_caches(ctx: &egui::Context, id: u64) {
    use std::sync::Arc;
    ctx.data_mut(|d| {
        d.remove::<(egui::Id, Arc<egui::Galley>)>(egui::Id::new(("editor_galley", id)));
        d.remove::<(egui::Id, Arc<egui::Galley>)>(egui::Id::new(("value_editor_galley", id)));
        d.remove::<(egui::Id, Vec<(usize, usize)>)>(egui::Id::new(("statement_ranges", id)));
        d.remove::<(egui::Id, bool)>(egui::Id::new(("previewable", id)));
        d.remove::<(egui::Id, Vec<(usize, usize)>)>(egui::Id::new(("result_find_cache", id)));
        d.remove::<(egui::Id, Arc<crate::values::ResultDiff>)>(egui::Id::new(("result_diff", id)));
        d.remove::<super::detail::DetailCache>(super::detail::cache_id(id));
        // The editor's own state: its undo history holds copies of the whole text.
        d.remove::<egui::text_edit::TextEditState>(egui::Id::new(("sql", id)));
    });
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

/// What the "Export" dropdown asked for.
#[derive(Clone, Copy)]
pub(super) enum ExportChoice {
    /// A file; `true`: the whole table, not just what is on screen.
    File(Format, bool),
    /// The result on screen, as text on the clipboard.
    Copy(Format),
}

/// Runs an "Export" choice on the tab.
pub(super) fn do_export(ui: &egui::Ui, tab: &mut Tab, s: &Session, choice: ExportChoice) {
    match choice {
        ExportChoice::File(format, whole) => app::start_export(tab, s, format, whole),
        ExportChoice::Copy(format) => {
            if let Some(text) = app::copy_result(tab, s, format) {
                ui.ctx().copy_text(text);
            }
        }
    }
}

/// "Export" dropdown: what is on screen, or (table tabs) the whole table. Returns the choice;
/// the caller starts the export once it can borrow the tab again.
pub(super) fn export_menu(
    ui: &mut egui::Ui,
    is_table: bool,
    enabled: bool,
    compact: bool,
) -> Option<ExportChoice> {
    let t = theme::tokens();
    let label = theme::tool(
        icon::DOWNLOAD_SIMPLE,
        "Export",
        compact,
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
            heading(ui, if is_table { "CURRENT PAGE" } else { "RESULT" });
            for f in Format::ALL {
                if ui.button(f.label()).clicked() {
                    chosen = Some(ExportChoice::File(f, false));
                    ui.close();
                }
            }
            ui.separator();
            heading(ui, "COPY TO CLIPBOARD");
            for f in [Format::Csv, Format::Json, Format::Sql] {
                if ui
                    .button(f.label())
                    .on_hover_text("The rows on screen as text, ready to paste")
                    .clicked()
                {
                    chosen = Some(ExportChoice::Copy(f));
                    ui.close();
                }
            }
            if is_table {
                ui.separator();
                heading(ui, "WHOLE TABLE (WITH FILTER)");
                for f in Format::ALL {
                    if ui.button(f.label()).clicked() {
                        chosen = Some(ExportChoice::File(f, true));
                        ui.close();
                    }
                }
            }
            ui.separator();
            ui.label(
                egui::RichText::new("Saved to the Downloads folder")
                    .size(11.0)
                    .color(t.subtle),
            );
        })
        .response
        .on_hover_text("Export");
    });
    chosen
}

/// "Exported N rows to file · Open folder · ×" after an export finishes.
pub(super) fn export_banner(ui: &mut egui::Ui, tab: &mut Tab) {
    if let Some((format, rows)) = tab.copied {
        let t = theme::tokens();
        let mut dismiss = false;
        egui::Frame::new()
            .fill(theme::mix(t.lime, 0.10, t.background))
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(theme::icon(icon::CHECK).color(t.lime));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} row(s) copied as {format}",
                            palette::format_count(rows as i64)
                        ))
                        .color(t.foreground),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        dismiss = ui
                            .add(
                                egui::Button::new(theme::icon(icon::X).size(11.0).color(t.subtle))
                                    .frame(false),
                            )
                            .clicked();
                    });
                });
            });
        if dismiss {
            tab.copied = None;
        }
        return;
    }
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
                        "{} row(s) exported to {name}",
                        palette::format_count(*rows as i64)
                    ))
                    .color(t.foreground),
                )
                .on_hover_text(path.display().to_string());
                if ui.link("Open folder").clicked() {
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

/// Spinner plus a button that stops the tab's running statement on the server.
pub(super) fn busy_indicator(ui: &mut egui::Ui, s: &Session, tab_id: u64) {
    let t = theme::tokens();
    ui.spinner();
    let stop = egui::Button::new(theme::icon_text(
        icon::STOP,
        "Cancel",
        t.destructive,
        t.foreground,
    ));
    if ui
        .add(stop)
        .on_hover_text("Stops the query on the server")
        .clicked()
    {
        s.cancel(tab_id);
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
        "Hide row details"
    } else {
        "Show row details"
    };
    if ui
        .button(theme::icon(icon::SIDEBAR_SIMPLE).color(color))
        .on_hover_text(tip)
        .clicked()
    {
        *show = !*show;
    }
}
