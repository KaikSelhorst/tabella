//! The result grid shared by editor and table tabs, and the dialogs it opens.

use super::*;
use std::fmt::Write as _;

/// Summary line for the pager: row count, timing and what the user can do with the grid.
pub(super) fn status_text(tab: &Tab, read_only: bool) -> String {
    let Some(res) = &tab.result else {
        return String::new();
    };
    let hint = match &tab.kind {
        TabKind::Table(_) if read_only => " · read-only connection",
        TabKind::Table(ts) if !ts.cols.is_empty() => {
            if ts.cols.iter().any(|c| c.pk) {
                " · double-click or Enter edits · Ctrl+S saves"
            } else {
                " · no primary key: read-only"
            }
        }
        _ => "",
    };
    format!("{}{hint}", summary(res))
}

/// "Result 1 · 2 · 3" above the grid when a script returned several result sets.
pub(super) fn result_sets_strip(ui: &mut egui::Ui, tab: &Tab) -> Option<usize> {
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
                        ui.label(egui::RichText::new("RESULTS").size(11.0).color(t.subtle));
                        for (i, set) in tab.sets.iter().enumerate() {
                            let shown = i == tab.set_shown;
                            let set = if shown {
                                tab.result.as_ref().unwrap_or(set)
                            } else {
                                set
                            };
                            let n = set.rows.len();
                            let label = if set.columns.is_empty() {
                                format!("#{} · {} affected", i + 1, set.affected.unwrap_or(0))
                            } else {
                                let unit = if n == 1 { "row" } else { "rows" };
                                format!("#{} · {} {unit}", i + 1, palette::compact_count(n as i64))
                            };
                            let color = if shown { t.foreground } else { t.muted_foreground };
                            let b = egui::Button::new(egui::RichText::new(label).size(12.0).color(color))
                                .selected(shown);
                            let tip = format!("{} column(s): {}", set.columns.len(), set.columns.join(", "));
                            if ui.add(b).on_hover_text(tip).clicked() {
                                pick = Some(i);
                            }
                        }
                    });
                });
        });
    pick
}

/// "5 statements" in the footer: opens the time each statement of the script took.
fn timings_menu(ui: &mut egui::Ui, timings: &[crate::db::StatementTime]) {
    let t = theme::tokens();
    let slowest = timings.iter().map(|s| s.elapsed).max().unwrap_or_default();
    let label = format!("{} statements ▾", timings.len());
    ui.menu_button(egui::RichText::new(label).small().color(t.lime), |ui| {
        egui::Grid::new("statement_times")
            .num_columns(4)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                for (i, st) in timings.iter().enumerate() {
                    let hot = st.elapsed == slowest;
                    let color = if hot { t.lime } else { t.muted_foreground };
                    ui.label(egui::RichText::new(format!("#{}", i + 1)).small().color(t.subtle));
                    ui.label(
                        egui::RichText::new(format!("line {}", st.line))
                            .small()
                            .color(t.subtle),
                    );
                    ui.label(
                        egui::RichText::new(format!("{} ms", st.elapsed.as_millis()))
                            .small()
                            .color(color),
                    );
                    let unit = if st.returned_rows { "row(s)" } else { "affected" };
                    ui.label(
                        egui::RichText::new(format!("{} {unit}", st.rows))
                            .small()
                            .color(t.subtle),
                    );
                    ui.label(egui::RichText::new(&st.sql).small().monospace());
                    ui.end_row();
                }
            });
    });
}

pub(super) fn summary(res: &QueryResult) -> String {
    if res.columns.is_empty() {
        format!(
            "{} row(s) affected · {} ms",
            res.affected.unwrap_or(0),
            res.elapsed.as_millis()
        )
    } else {
        let truncated = if res.truncated { " · result truncated" } else { "" };
        format!(
            "{} row(s) · {} ms{truncated}",
            res.rows.len(),
            res.elapsed.as_millis()
        )
    }
}

pub(super) fn result_area(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx, editable: bool) {
    let s = cx.s;
    stats_dialog(ui.ctx(), tab, s);
    if let Some(e) = value_dialog(ui.ctx(), tab) {
        // Saved: a pending change like any other edit.
        if let Some(Effect::Copy(text)) = app::handle_grid_event(tab, s, e, true) {
            ui.ctx().copy_text(text);
        }
    }
    value_filter_dialog(ui.ctx(), tab, s);
    // A value picked for a cell is an edit like any other (insert-form picks are the form's).
    if tab.value_editor.is_none()
        && !matches!(&tab.kind, TabKind::Table(ts) if ts.insert.is_some())
        && let Some((app::PickFor::Cell(row, col), key)) = fk_dialog(ui.ctx(), tab, s)
    {
        app::handle_grid_event(tab, s, GridEvent::Commit(row, col, Some(key)), true);
    }
    if let Some(sql) = bulk_dialog(ui.ctx(), tab, s) {
        cx.request = Some(Request::PreviewUpdate(sql));
    }
    if let Some(e) = fill_dialog(ui.ctx(), tab) {
        app::handle_grid_event(tab, s, e, true);
    }
    let t = theme::tokens();
    // "Load more" in the footer sets this; the grid below is still borrowing the result.
    let more_id = egui::Id::new(("load_more", tab.id));
    if ui.data_mut(|d| d.remove_temp::<bool>(more_id)).unwrap_or(false) {
        app::load_more(tab, s);
    }
    let can_more = app::can_load_more(tab);
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
                ui.label(egui::RichText::new("Loading…").color(t.subtle));
            });
        } else if !editable && tab.error.is_none() {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.label(theme::icon(icon::PLAY).size(20.0).color(t.subtle));
                ui.label(egui::RichText::new("Run a query to see the result here").color(t.subtle));
            });
        }
        return;
    };
    // Editor results can be charted (kept in egui memory per tab).
    let is_sql = matches!(tab.kind, TabKind::Sql(_));
    // Editor results from one table say whether they can be edited, and why not.
    let edit_hint = match tab.editor().and_then(|ed| ed.source.as_deref()) {
        Some(src) if !src.cols.is_empty() => {
            let pk = src.pk_names();
            if s.cfg.read_only {
                None
            } else if pk.is_empty() {
                Some(format!("· {} has no primary key: read-only", src.table))
            } else if pk.iter().all(|p| res.columns.contains(p)) {
                Some(format!(
                    "· editable ({}) · double-click edits · Ctrl+S saves",
                    src.table
                ))
            } else {
                Some(format!(
                    "· include {} in the SELECT to be able to edit",
                    pk.join(", ")
                ))
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
    let (pinned_at, comparing) = match &tab.kind {
        TabKind::Sql(ed) => (ed.pinned.as_ref().map(|p| p.at), ed.comparing),
        _ => (None, false),
    };
    let mut pin_action: Option<PinAction> = None;
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
                    if tab.timings.len() > 1 {
                        timings_menu(ui, &tab.timings);
                    }
                    if can_more {
                        let more = egui::Button::new(
                            egui::RichText::new(format!(
                                "Load {} more",
                                crate::ui::palette::format_count(crate::db::max_rows() as i64)
                            ))
                            .small()
                            .color(t.lime),
                        )
                        .frame(false);
                        if ui
                            .add(more)
                            .on_hover_text("Runs the query again keeping more rows")
                            .clicked()
                        {
                            ui.data_mut(|d| d.insert_temp(more_id, true));
                            ui.ctx().request_repaint();
                        }
                    }
                    if let Some(hint) = &edit_hint {
                        ui.label(egui::RichText::new(hint).small().color(t.subtle));
                    }
                    let chartable =
                        is_sql && chart::default_spec(res, &result_kinds).is_some() && !res.rows.is_empty();
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if is_sql {
                            pin_action = pin_buttons(ui, pinned_at, comparing);
                        }
                        if chartable {
                            let on = chart_spec.is_some();
                            let color = if on { t.lime } else { t.muted_foreground };
                            let b = egui::Button::new(theme::icon_text(
                                icon::CHART_BAR,
                                "Chart",
                                color,
                                t.foreground,
                            ))
                            .frame(false);
                            if ui
                                .add(b)
                                .on_hover_text("Bars or line of a numeric column")
                                .clicked()
                            {
                                chart_spec = if on {
                                    None
                                } else {
                                    chart::default_spec(res, &result_kinds)
                                };
                            }
                        }
                    });
                });
            });
    }
    // Pin / compare / unpin, then the comparison in place of the grid while it is on.
    if let TabKind::Sql(ed) = &mut tab.kind {
        match pin_action {
            Some(PinAction::Pin) => {
                let key = ed.source.as_ref().and_then(|src| {
                    let pk = src.pk_names();
                    (pk.len() == 1).then(|| pk[0].clone())
                });
                ed.pin(res, key);
            }
            Some(PinAction::Compare(on)) => ed.comparing = on,
            Some(PinAction::Unpin) => {
                ed.pinned = None;
                ed.comparing = false;
            }
            None => {}
        }
        if ed.comparing
            && let Some(p) = &ed.pinned
        {
            let done =
                super::result_diff::diff_view(ui, tab.id, p, res, &mut ed.compare_key, &mut ed.hide_equal);
            if done {
                ed.comparing = false;
            }
            return;
        }
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
    let overlay = app::editable_ref(&tab.kind).and_then(|ts| ts.overlay(res));
    // Cells saved in the open transaction, not committed yet.
    let uncommitted = app::editable_ref(&tab.kind)
        .filter(|_| !s.uncommitted.is_empty())
        .map(|ts| s.uncommitted_in(ts, res))
        .filter(|c| !c.is_empty());
    let shown = overlay.as_ref().map_or(res, |o| &o.result);
    let view = tab.view();
    let pinned = tab.pinned_count();
    let range = tab.range().filter(|r| r.r1 < shown.rows.len());
    let no_cols = Vec::new();
    let no_incoming = Vec::new();
    let table_tab = matches!(tab.kind, TabKind::Table(_));
    let filtered: Vec<String> = match &tab.kind {
        TabKind::Table(ts) => ts.value_conds.iter().map(|v| v.column.clone()).collect(),
        _ => Vec::new(),
    };
    let (cols, edit, order, option_pick, row_offset, has_pk, incoming) =
        match app::editable_mut(&mut tab.kind) {
            Some(ts) => {
                // Rows are addressed by their key: every key column must be in the result.
                let pk = ts.pk_names();
                let has_pk = !pk.is_empty() && pk.iter().all(|p| res.columns.contains(p));
                (
                    &ts.cols,
                    ts.edit.as_mut(),
                    ts.order.clone(),
                    ts.option_pick,
                    if table_tab { ts.page * page_size() } else { 0 },
                    has_pk,
                    &ts.incoming,
                )
            }
            None => (&no_cols, None, Vec::new(), None, 0, false, &no_incoming),
        };
    let can_edit = has_pk && !s.cfg.read_only;
    let kinds = values::column_kinds(res, cols);
    let selected = tab
        .selected
        .filter(|(r, c)| *r < res.rows.len() && *c < res.columns.len());
    let editing = edit.is_some() || option_pick.is_some();

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
        option_pick,
        widths: table_tab.then_some(&tab.col_widths),
        kinds: &kinds,
        changed: overlay.as_ref().map(|o| &o.changed),
        uncommitted: uncommitted.as_ref(),
        deleted: overlay.as_ref().map(|o| &o.deleted),
        can_filter: editable,
        range: range.as_ref(),
        view: &view,
        pinned,
        found: found_set.as_ref(),
        found_current,
        incoming,
        filtered: &filtered,
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
            ui.label(egui::RichText::new("No rows").color(t.subtle));
        });
    }

    if let (Some(a), Some((row, _))) = (detail_event, selected) {
        match a {
            DetailAction::Commit(ci, v) => event = Some(GridEvent::Commit(row, ci, v)),
            DetailAction::Copy(how) => event = Some(GridEvent::Copy(row, how)),
            DetailAction::Open(ci) => event = Some(GridEvent::OpenValue(row, ci)),
            DetailAction::PickFk(ci) => event = Some(GridEvent::StartEdit(row, ci)),
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
        Some(Effect::ColumnWidths {
            schema,
            table,
            widths,
        }) => {
            cx.request = Some(Request::ColumnWidths(schema, table, widths));
        }
        None => {}
    }
}

/// Pinned columns (and the row numbers) drawn again at the left edge once the grid is scrolled
/// sideways past them. Clicks on them select, double-clicks edit, like the cells underneath.
pub(super) fn paint_frozen(
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
            if r.double_clicked() || r.triple_clicked() {
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
pub(super) fn found_cells(
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
                if let Some(v) = row.cell(ci)
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

/// "Set value in the N cells": type the value (or choose NULL).
/// The UPDATE the "Set with an expression" form describes, or what is wrong with it.
fn bulk_sql(tab: &Tab, s: &Session, p: &app::BulkPrompt) -> Result<String, String> {
    let (TabKind::Table(ts), Some(res)) = (&tab.kind, &tab.result) else {
        return Err("Open a table first".into());
    };
    sql::check_expression(&p.expression)?;
    let k = s.cfg.kind;
    let pk = ts.pk_names();
    if p.selected_only {
        let sel = tab.selection().ok_or("Select the rows first")?;
        let rows: Vec<Vec<Option<String>>> = (sel.r0..=sel.r1).map(|r| ts.row_key(res, r)).collect();
        if pk.is_empty() {
            return Err("This table has no primary key to name the rows by".into());
        }
        Ok(sql::bulk_update(
            k,
            &ts.schema,
            &ts.table,
            &p.column,
            &p.expression,
            &sql::BulkScope::Keys { pk: &pk, rows: &rows },
        ))
    } else {
        Ok(sql::bulk_update(
            k,
            &ts.schema,
            &ts.table,
            &p.column,
            &p.expression,
            &sql::BulkScope::Filter(&ts.effective_filter(k)),
        ))
    }
}

/// "Set <column> with an expression": which column, the new value as SQL, and which rows. "Preview"
/// opens the UPDATE in an editor and shows what it would change; nothing is written from here.
pub(super) fn bulk_dialog(ctx: &egui::Context, tab: &mut Tab, s: &Session) -> Option<String> {
    tab.bulk_prompt.as_ref()?;
    let t = theme::tokens();
    let selected_rows = tab.selection().map_or(0, |r| r.r1 - r.r0 + 1);
    let TabKind::Table(ts) = &tab.kind else {
        tab.bulk_prompt = None;
        return None;
    };
    let columns: Vec<String> = ts.cols.iter().filter(|c| !c.pk).map(|c| c.name.clone()).collect();
    let (total, filtered) = (ts.total, !ts.effective_filter(s.cfg.kind).trim().is_empty());
    let mut prompt = tab.bulk_prompt.take()?;
    let built = bulk_sql(tab, s, &prompt);
    let (mut preview, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new(("bulk", tab.id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(480.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::LIGHTNING).size(16.0).color(t.lime));
                ui.label(
                    egui::RichText::new("Set a column with an expression")
                        .size(15.0)
                        .color(t.foreground),
                );
            });
            ui.add_space(8.0);
            egui::Grid::new("bulk_form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Column");
                egui::ComboBox::from_id_salt("bulk_col")
                    .selected_text(&prompt.column)
                    .show_ui(ui, |ui| {
                        for c in &columns {
                            ui.selectable_value(&mut prompt.column, c.clone(), c);
                        }
                    });
                ui.end_row();
                ui.label("New value");
                let r = ui.add(
                    TextEdit::singleline(&mut prompt.expression)
                        .hint_text("price * 1.1   ·   upper(name)   ·   NULL   ·   'inactive'")
                        .font(egui::FontId::monospace(12.5))
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    preview = true;
                }
                ui.end_row();
                ui.label("Rows");
                ui.vertical(|ui| {
                    ui.add_enabled_ui(selected_rows > 0, |ui| {
                        ui.radio_value(
                            &mut prompt.selected_only,
                            true,
                            format!("The {selected_rows} selected row(s)"),
                        );
                    });
                    let all = match (total, filtered) {
                        (Some(n), true) => format!("Every row matching the filter ({} rows)", palette::format_count(n)),
                        (Some(n), false) => format!("Every row of the table ({} rows)", palette::format_count(n)),
                        (None, true) => "Every row matching the filter".to_string(),
                        (None, false) => "Every row of the table".to_string(),
                    };
                    ui.radio_value(&mut prompt.selected_only, false, all);
                });
                ui.end_row();
            });
            ui.add_space(8.0);
            match &built {
                Ok(sql) => {
                    egui::Frame::new()
                        .fill(t.sidebar)
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            // A long list of keys is cut: the editor shows the whole statement.
                            let shown: String = sql.chars().take(400).collect();
                            let more = if sql.chars().count() > 400 { " …" } else { "" };
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("{shown}{more}"))
                                        .monospace()
                                        .size(12.0)
                                        .color(t.foreground),
                                )
                                .wrap(),
                            );
                        });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "Preview opens this UPDATE in an editor and shows how many rows and which ones it would \
                             change. Nothing is written until you run it there.",
                        )
                        .size(11.0)
                        .color(t.subtle),
                    );
                }
                Err(e) => {
                    ui.label(egui::RichText::new(e).size(12.0).color(t.muted_foreground));
                }
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(built.is_ok(), theme::primary_button(None, "Preview"))
                    .clicked()
                {
                    preview = true;
                }
                if ui.add(theme::outline_button("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
    if preview && let Ok(sql) = built {
        return Some(sql);
    }
    if !(cancel || modal.should_close()) {
        tab.bulk_prompt = Some(prompt);
    }
    None
}

pub(super) fn fill_dialog(ctx: &egui::Context, tab: &mut Tab) -> Option<GridEvent> {
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
                egui::RichText::new(format!("Value for {cells} cell(s)"))
                    .size(15.0)
                    .color(t.foreground),
            );
            ui.label(
                egui::RichText::new("Stays pending until saved (Ctrl+S), like the other edits.")
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
                if ui.add(theme::primary_button(None, "Apply")).clicked() {
                    out = Some(GridEvent::FillSelection(Some(value.clone())));
                }
                if ui.add(theme::outline_button("NULL")).clicked() {
                    out = Some(GridEvent::FillSelection(None));
                }
                if ui.add(theme::outline_button("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
    if out.is_none() && (cancel || modal.should_close()) {
        tab.fill_prompt = None;
    }
    out
}

/// "Column statistics": fill rate, distinct values, range and the most common values, each
/// one a click away from becoming a filter (table tabs).
pub(super) fn stats_dialog(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
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
                        ui.label(egui::RichText::new("Counting…").color(t.muted_foreground));
                    });
                }
                Some(Err(e)) => {
                    ui.label(egui::RichText::new(e).color(t.destructive));
                }
                Some(Ok(st)) => {
                    let pct = |n: i64| {
                        if st.total > 0 {
                            format!("{:.1}%", n as f64 * 100.0 / st.total as f64)
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
                            row(ui, "Rows", palette::format_count(st.total));
                            row(
                                ui,
                                "Nulls",
                                format!("{} · {}", palette::format_count(st.nulls), pct(st.nulls)),
                            );
                            row(ui, "Distinct", palette::format_count(st.distinct));
                            if let Some(v) = &st.min {
                                row(ui, "Minimum", v.clone());
                            }
                            if let Some(v) = &st.max {
                                row(ui, "Maximum", v.clone());
                            }
                        });
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new("MOST COMMON VALUES")
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
                                        .on_hover_text("Filter the table by this value")
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
                        ui.label(egui::RichText::new("No rows").color(t.subtle));
                    }
                }
            }
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(theme::outline_button("Close")).clicked() {
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
pub(super) fn keyboard_event(
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
        } else if i.modifiers.shift && i.key_pressed(Key::Enter) {
            Some(GridEvent::OpenValue(row, col))
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

/// "6 cells · 4 values · sum 5.5 · avg 2.75 · min 1.5 · max 4" under a range selection.
pub(super) fn selection_stats(
    ui: &mut egui::Ui,
    tab_id: u64,
    shown: &QueryResult,
    kinds: &[values::ValueKind],
    range: &values::CellRange,
) {
    let t = theme::tokens();
    let st = values::stats(shown, kinds, range);
    let mut parts = vec![format!("{} cells", st.cells), format!("{} values", st.values)];
    if let Some((n, sum, lo, hi)) = st.numbers {
        let f = values::format_number;
        parts.push(format!("sum {}", f(sum)));
        parts.push(format!("avg {}", f(sum / n as f64)));
        parts.push(format!("min {}", f(lo)));
        parts.push(format!("max {}", f(hi)));
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
                        .on_hover_text("Copy summary")
                        .clicked()
                    {
                        ui.ctx().copy_text(text.clone());
                    }
                });
            });
        });
}

/// "Filter values": check the values to keep; the list comes from the database.
pub(super) fn value_filter_dialog(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let id = tab.id;
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let table = ts.table.clone();
    let has_cond = |col: &str| ts.value_conds.iter().any(|v| v.column == col);
    let Some(vf) = &ts.value_filter else { return };
    let had = has_cond(&vf.column);
    let Some(vf) = &mut ts.value_filter else { return };
    let t = theme::tokens();
    let (mut apply, mut cancel, mut clear, mut search_db) = (false, false, false, false);
    let modal = egui::Modal::new(egui::Id::new(("value_filter", id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(420.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::FUNNEL).size(16.0).color(t.lime));
                ui.label(egui::RichText::new(&vf.column).size(15.0).color(t.foreground));
                ui.label(
                    egui::RichText::new(format!("in {table}"))
                        .size(11.0)
                        .color(t.subtle),
                );
            });
            ui.add_space(8.0);
            let r = ui.add(
                TextEdit::singleline(&mut vf.search)
                    .hint_text(if vf.complete {
                        "Search the list"
                    } else {
                        "Search (Enter searches the database)"
                    })
                    .desired_width(f32::INFINITY),
            );
            if !vf.focused {
                r.request_focus();
                vf.focused = true;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !vf.complete {
                search_db = true;
            }
            ui.add_space(8.0);
            let values = match &vf.values {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Reading the values…").color(t.muted_foreground));
                    });
                    None
                }
                Some(Err(e)) => {
                    ui.label(egui::RichText::new(e).color(t.destructive));
                    None
                }
                Some(Ok(v)) => Some(v),
            };
            let mut can_apply = false;
            if let Some(values) = values {
                let needle = vf.search.trim().to_lowercase();
                let shown: Vec<&(Option<String>, i64)> = values
                    .iter()
                    .filter(|(v, _)| {
                        needle.is_empty() || v.as_deref().unwrap_or("null").to_lowercase().contains(&needle)
                    })
                    .collect();
                // "Select all" acts on what the search shows.
                let all_on = shown.iter().all(|(v, _)| !vf.unchecked.contains(v));
                let mut all = all_on;
                let label = if needle.is_empty() {
                    "Select all"
                } else {
                    "Select the matches"
                };
                if ui.checkbox(&mut all, label).changed() {
                    for (v, _) in &shown {
                        if all {
                            vf.unchecked.remove(v);
                        } else {
                            vf.unchecked.insert(v.clone());
                        }
                    }
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (v, n) in &shown {
                            let mut on = !vf.unchecked.contains(v);
                            ui.horizontal(|ui| {
                                let text = match v {
                                    Some(v) => egui::RichText::new(v).monospace().color(t.foreground),
                                    None => egui::RichText::new("NULL").monospace().italics().color(t.subtle),
                                };
                                if ui.checkbox(&mut on, text).changed() {
                                    if on {
                                        vf.unchecked.remove(v);
                                    } else {
                                        vf.unchecked.insert(v.clone());
                                    }
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        egui::RichText::new(palette::format_count(*n))
                                            .size(11.0)
                                            .color(t.subtle),
                                    );
                                });
                            });
                        }
                        if shown.is_empty() {
                            ui.label(egui::RichText::new("No values found").color(t.subtle));
                        }
                    });
                ui.separator();
                let kept = values.iter().filter(|(v, _)| !vf.unchecked.contains(v)).count();
                can_apply = kept > 0;
                let mut note = format!("{kept} of {} checked", values.len());
                if !vf.complete {
                    let _ = write!(
                        note,
                        " · only the {} most common: the others are left out when applying",
                        sql::DISTINCT_LIMIT
                    );
                }
                ui.label(egui::RichText::new(note).size(11.0).color(t.subtle));
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                apply = ui
                    .add_enabled(can_apply, theme::primary_button(Some(icon::FUNNEL), "Apply"))
                    .on_disabled_hover_text("Check at least one value")
                    .clicked();
                cancel = ui.add(theme::outline_button("Cancel")).clicked();
                if had {
                    clear = ui.add(theme::outline_button("Remove filter")).clicked();
                }
            });
        });
    if apply {
        app::apply_value_filter(tab, s);
    } else if clear {
        vf.unchecked.clear();
        app::apply_value_filter(tab, s);
    } else if search_db {
        app::load_values(tab, s);
    } else if cancel || modal.should_close() {
        ts.value_filter = None;
    }
}

/// The big editor for one value: JSON laid out, colored and checked; long text with room.
/// Returns the edit to make when saved.
pub(super) fn value_dialog(ctx: &egui::Context, tab: &mut Tab) -> Option<GridEvent> {
    let id = tab.id;
    let ve = tab.value_editor.as_mut()?;
    let t = theme::tokens();
    let screen = ctx.content_rect();
    let json = ve.json_column || ve.text.trim_start().starts_with(['{', '[']);
    let check = (json && !ve.null).then(|| values::json_check(&ve.text));
    let changed = ve.value() != ve.original;
    let can_save = ve.editable && changed && !(ve.json_column && check.as_ref().is_some_and(Result::is_err));
    let (mut save, mut close) = (false, false);
    let modal = egui::Modal::new(egui::Id::new(("value_editor", id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width((screen.width() * 0.7).clamp(480.0, 1100.0));
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::ARROWS_OUT_SIMPLE).size(16.0).color(t.lime));
                ui.label(egui::RichText::new(&ve.column).size(15.0).color(t.foreground));
                ui.label(egui::RichText::new(&ve.data_type).size(11.0).color(t.subtle));
                ui.label(
                    egui::RichText::new(format!("row {}", ve.row + 1))
                        .size(11.0)
                        .color(t.subtle),
                );
                if !ve.editable {
                    ui.label(egui::RichText::new("· read-only").size(11.0).color(t.subtle));
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let valid = check.as_ref().is_some_and(Result::is_ok);
                if json && ve.editable && !ve.null {
                    if ui.add_enabled(valid, egui::Button::new("Format")).clicked() {
                        ve.text = values::json_pretty(&ve.text);
                    }
                    if ui.add_enabled(valid, egui::Button::new("Compact")).clicked() {
                        ve.text = values::json_compact(&ve.text);
                    }
                }
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(ve.text.clone());
                }
                if ve.editable {
                    ui.checkbox(&mut ve.null, "NULL")
                        .on_hover_text("Save the cell as NULL");
                }
            });
            ui.add_space(4.0);
            let cache = egui::Id::new(("value_editor_galley", id));
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                // A big document is colored once, not on every frame.
                let key = egui::Id::new((buf.as_str(), wrap.to_bits(), json, theme::is_light()));
                if let Some((k, g)) =
                    ui.data(|d| d.get_temp::<(egui::Id, std::sync::Arc<egui::Galley>)>(cache))
                    && k == key
                {
                    return g;
                }
                let font = egui::FontId::monospace(12.5);
                let mut job = if json {
                    syntax::highlight_json(buf.as_str(), &font)
                } else {
                    egui::text::LayoutJob::simple(buf.as_str().to_string(), font, t.foreground, wrap)
                };
                job.wrap.max_width = wrap;
                let g = ui.fonts_mut(|f| f.layout_job(job));
                ui.data_mut(|d| d.insert_temp(cache, (key, g.clone())));
                g
            };
            egui::ScrollArea::vertical()
                .max_height(screen.height() * 0.6)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let edit = if ve.editable && !ve.null {
                        TextEdit::multiline(&mut ve.text)
                    } else {
                        // Selectable and copyable, not editable.
                        let mut shown = if ve.null { "NULL" } else { ve.text.as_str() };
                        return ui.add(
                            TextEdit::multiline(&mut shown)
                                .code_editor()
                                .desired_rows(16)
                                .desired_width(f32::INFINITY)
                                .layouter(&mut layouter),
                        );
                    };
                    ui.add(
                        edit.code_editor()
                            .desired_rows(16)
                            .desired_width(f32::INFINITY)
                            .layouter(&mut layouter),
                    )
                });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let lines = ve.text.lines().count().max(1);
                let size = format!("{} characters · {lines} line(s)", ve.text.chars().count());
                ui.label(egui::RichText::new(size).size(11.0).color(t.subtle));
                match &check {
                    Some(Ok(())) => {
                        ui.label(egui::RichText::new("· valid JSON").size(11.0).color(t.lime));
                    }
                    Some(Err((line, col, _))) => {
                        let msg = format!("· invalid JSON: line {line}, column {col}");
                        ui.label(egui::RichText::new(msg).size(11.0).color(t.destructive));
                    }
                    None => {}
                }
                if ve.compact && ve.editable {
                    ui.label(
                        egui::RichText::new("· saved compact again, as it was")
                            .size(11.0)
                            .color(t.subtle),
                    );
                }
            });
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ve.editable {
                    let why = if !changed {
                        "Nothing changed"
                    } else {
                        "JSON column: fix the JSON before saving"
                    };
                    save = ui
                        .add_enabled(can_save, theme::primary_button(None, "Save"))
                        .on_hover_text("Stays pending, like the other edits (Ctrl+S writes it)")
                        .on_disabled_hover_text(why)
                        .clicked();
                }
                let label = if ve.editable { "Cancel" } else { "Close" };
                close = ui.add(theme::outline_button(label)).clicked();
            });
        });
    let event = save.then(|| GridEvent::Commit(ve.row, ve.col, ve.value()));
    // Esc or a click outside closes it only when nothing would be lost.
    if save || close || (modal.should_close() && !changed) {
        tab.value_editor = None;
    }
    event
}

enum PinAction {
    Pin,
    Compare(bool),
    Unpin,
}

/// "Pin" while nothing is pinned; then "Compare", "Pin this" and unpin.
fn pin_buttons(ui: &mut egui::Ui, pinned_at: Option<u64>, comparing: bool) -> Option<PinAction> {
    let t = theme::tokens();
    let mut action = None;
    let button = |glyph: &str, text: &str, color: egui::Color32| {
        egui::Button::new(theme::icon_text(glyph, text, color, t.foreground)).frame(false)
    };
    match pinned_at {
        None => {
            if ui
                .add(button(icon::PUSH_PIN, "Pin", t.muted_foreground))
                .on_hover_text("Keeps this result to compare with the next ones")
                .clicked()
            {
                action = Some(PinAction::Pin);
            }
        }
        Some(at) => {
            let unpin =
                egui::Button::new(theme::icon(icon::PUSH_PIN_SLASH).color(t.muted_foreground)).frame(false);
            if ui.add(unpin).on_hover_text("Unpin").clicked() {
                action = Some(PinAction::Unpin);
            }
            if !comparing
                && ui
                    .add(button(icon::PUSH_PIN, "Pin this", t.muted_foreground))
                    .on_hover_text("Replaces the pinned result with this one")
                    .clicked()
            {
                action = Some(PinAction::Pin);
            }
            let color = if comparing { t.lime } else { t.muted_foreground };
            let when = crate::history::ago(at, crate::history::now());
            if ui
                .add(button(icon::GIT_DIFF, "Compare with the pinned one", color))
                .on_hover_text(format!("Pinned {when}: new, removed and changed rows"))
                .clicked()
            {
                action = Some(PinAction::Compare(!comparing));
            }
        }
    }
    action
}

/// "Escolher" a foreign key's value. Returns where it goes and the key chosen.
pub(super) fn fk_dialog(ctx: &egui::Context, tab: &mut Tab, s: &Session) -> Option<(app::PickFor, String)> {
    let id = tab.id;
    let p = tab.fk_picker.as_mut()?;
    let t = theme::tokens();
    let mut chosen: Option<String> = None;
    let modal = egui::Modal::new(egui::Id::new(("fk_picker", id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(520.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::MAGNIFYING_GLASS).size(16.0).color(t.lime));
                ui.label(egui::RichText::new(&p.column).size(15.0).color(t.foreground));
                ui.label(
                    egui::RichText::new(format!("→ {}.{}", p.fk.table, p.fk.column))
                        .size(11.0)
                        .color(t.subtle),
                );
            });
            ui.add_space(8.0);
            let hint = match (&p.label, p.numeric_key) {
                (Some(l), true) => format!("Search by {l} or by the number of {}", p.fk.column),
                (Some(l), false) => format!("Search by {l} or {}", p.fk.column),
                (None, _) => format!("Search by {}", p.fk.column),
            };
            let r = ui.add(
                TextEdit::singleline(&mut p.search)
                    .hint_text(hint)
                    .desired_width(f32::INFINITY),
            );
            if !p.focused {
                r.request_focus();
                p.focused = true;
            }
            if r.changed() {
                p.typed_at = std::time::Instant::now();
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.add_space(6.0);
            let needle = p.search.trim().to_lowercase();
            let shown: Vec<&(String, Option<String>)> = match &p.options {
                Some(Ok(rows)) => rows
                    .iter()
                    .filter(|(k, l)| {
                        !p.local
                            || needle.is_empty()
                            || k.to_lowercase().contains(&needle)
                            || l.as_deref().is_some_and(|l| l.to_lowercase().contains(&needle))
                    })
                    .take(200)
                    .collect(),
                _ => Vec::new(),
            };
            egui::ScrollArea::vertical()
                .max_height(340.0)
                .auto_shrink([false, true])
                .show(ui, |ui| match &p.options {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new("Loading…").color(t.muted_foreground));
                        });
                    }
                    Some(Err(e)) => {
                        ui.label(egui::RichText::new(e).color(t.destructive));
                    }
                    Some(Ok(_)) if shown.is_empty() => {
                        ui.label(egui::RichText::new("Nothing found").color(t.subtle));
                    }
                    Some(Ok(_)) => {
                        // Keys padded to the same width (monospace), so the labels line up.
                        let key_width = shown.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
                        for (key, label) in &shown {
                            let current = p.current.as_deref() == Some(key.as_str());
                            let mut text = egui::text::LayoutJob::default();
                            let mono = egui::FontId::monospace(12.5);
                            text.append(
                                &format!("{key:<key_width$}"),
                                0.0,
                                egui::TextFormat::simple(mono.clone(), t.number),
                            );
                            if let Some(l) = label {
                                text.append(
                                    &format!("   {l}"),
                                    0.0,
                                    egui::TextFormat::simple(egui::FontId::proportional(13.0), t.foreground),
                                );
                            }
                            let r = ui.add(
                                egui::Button::selectable(current, text)
                                    .min_size(egui::vec2(ui.available_width(), 24.0)),
                            );
                            if r.clicked() {
                                chosen = Some(key.clone());
                            }
                        }
                    }
                });
            if enter && let Some((k, _)) = shown.first() {
                chosen = Some(k.clone());
            }
            ui.add_space(6.0);
            let note = match (&p.options, p.local) {
                (Some(Ok(rows)), true) if rows.len() as i64 > crate::sql::FK_LOCAL_ROWS => {
                    "Large table: type to filter".to_string()
                }
                (Some(Ok(_)), true) => format!("{} · filters as you type", p.fk.table),
                (_, false) if p.loading => "Searching…".to_string(),
                (_, false) => format!(
                    "{} is large: searches the database, up to {} results",
                    p.fk.table,
                    crate::sql::FK_SEARCH_ROWS
                ),
                _ => String::new(),
            };
            ui.label(egui::RichText::new(note).size(11.0).color(t.subtle));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(theme::outline_button("Cancel")).clicked() {
                    p.focused = false;
                    chosen = Some(String::new());
                }
            });
        });
    // Database search once typing pauses (not on every key).
    if !p.local && p.search != p.asked {
        let wait = std::time::Duration::from_millis(300);
        let quiet = p.typed_at.elapsed();
        if quiet >= wait && !p.loading {
            app::fk_search(tab, s);
            return None;
        }
        ctx.request_repaint_after(wait.saturating_sub(quiet));
    }
    let p = tab.fk_picker.as_ref()?;
    let target = p.target;
    match chosen {
        // "Cancel" (empty) or Esc: nothing chosen.
        Some(k) if k.is_empty() => {
            tab.fk_picker = None;
            None
        }
        Some(k) => {
            tab.fk_picker = None;
            Some((target, k))
        }
        None if modal.should_close() => {
            tab.fk_picker = None;
            None
        }
        None => None,
    }
}
