//! The SQL editor tab: toolbar, editor, parameters, previews and confirmations before running.

use super::*;

/// Room the editor toolbar keeps on its right for the panel toggle and "on database · schema".
const RIGHT_ROOM: f32 = 200.0;

pub(super) fn sql_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let busy = tab.busy();
    let can_export = !busy && tab.result.as_ref().is_some_and(|r| !r.columns.is_empty());
    let mut export = None;
    let TabKind::Sql(ed) = &mut tab.kind else {
        return;
    };
    let last_reads = ed.last_reads;
    let mut repeat_value = ed.repeat;
    let repeat = &mut repeat_value;
    let has_file = ed.file.is_some();
    let error_jump = std::mem::take(&mut ed.error_jump);
    let (text, error_at) = (&mut ed.text, &mut ed.error_at);
    let mut run: Option<String> = None;
    let mut explain: Option<(String, bool)> = None;
    let mut preview: Option<String> = None;
    let mut go_to: Option<Request> = None;
    let mut file_menu_action: Option<Request> = None;
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
            // Selection survives losing focus (e.g. clicking Run), so read it from the stored state.
            let cursor = TextEdit::load_state(ui.ctx(), editor_id).and_then(|st| st.cursor.char_range());
            let selected_range = cursor
                .map(|r| r.as_sorted_char_range())
                .map(|r| (usize::from(r.start), usize::from(r.end)))
                .filter(|(a, b)| a < b);
            let chars = |(a, b): (usize, usize)| text.chars().skip(a).take(b - a).collect::<String>();
            let selection = selected_range.map(chars);
            // With several statements, Ctrl+Enter runs the one under the cursor and F5 runs all.
            // Splitting a long script takes a while: done again only when the text changes.
            let ranges = statement_ranges_cached(ui.ctx(), tab.id, text);
            let several = ranges.len() > 1;
            let current = if several && selection.is_none() {
                cursor.and_then(|c| sql::statement_in(&ranges, usize::from(c.primary.index)))
            } else {
                None
            };
            let statement = current.map(chars);
            // What Ctrl+Enter / the main button runs, and what F5 / "Run all" runs.
            let one = selection
                .clone()
                .or_else(|| statement.clone())
                .unwrap_or_else(|| text.clone());
            let all = selection.clone().unwrap_or_else(|| text.clone());
            let can_preview = previewable_cached(ui.ctx(), tab.id, &one);
            let format_key =
                ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT | egui::Modifiers::ALT, Key::F));
            ui.horizontal(|ui| {
                // Short of room (narrow window, big zoom): icons only. Decided from the width the
                // full bar took when it last fitted, so it doesn't flip back and forth.
                let start_x = ui.cursor().left();
                let avail = ui.available_width();
                let variant = egui::Id::new(("editor_toolbar", statement.is_some(), can_preview, repeat.is_some(), busy));
                let full: Option<f32> = ui.data(|d| d.get_temp(variant));
                let compact = full.is_some_and(|w| w + RIGHT_ROOM > avail);
                let label = if selection.is_some() && !compact {
                    "Run selection"
                } else if statement.is_some() && !compact {
                    "Run statement"
                } else {
                    "Run"
                };
                if ui
                    .add_enabled(!busy, theme::primary_button(Some(icon::PLAY), label))
                    .on_hover_text(if statement.is_some() {
                        "Ctrl+Enter · the statement under the cursor"
                    } else {
                        "Ctrl+Enter or F5"
                    })
                    .clicked()
                {
                    run = Some(one.clone());
                }
                if statement.is_some()
                    && ui
                        .add_enabled(!busy, theme::outline_button(if compact { "All" } else { "Run all" }))
                        .on_hover_text("F5 · every statement, one result per query")
                        .clicked()
                {
                    run = Some(all.clone());
                }
                if can_preview
                    && ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(theme::tool(icon::EYE, "Preview", compact, t.muted_foreground, t.foreground)),
                        )
                        .on_hover_text("Ctrl+Shift+Enter · how many and which rows this UPDATE/DELETE would change, without changing them")
                        .clicked()
                {
                    preview = Some(one.clone());
                }
                if busy {
                    busy_indicator(ui, s);
                }
                match history_menu(ui, cx.history, &s.cfg.name, compact) {
                    Some(HistoryPick::Load(sql)) => *text = sql,
                    Some(HistoryPick::All) => file_menu_action = Some(Request::History),
                    None => {}
                }
                match favorites_menu(ui, cx.favorites, &s.cfg.id, !text.trim().is_empty(), compact) {
                    Some(FavAction::Load(sql)) => *text = sql,
                    Some(FavAction::Save) => {
                        let sql = selection.clone().unwrap_or_else(|| text.clone());
                        cx.request = Some(Request::SaveFavorite(sql));
                    }
                    Some(FavAction::Delete(id)) => cx.request = Some(Request::DeleteFavorite(id)),
                    None => {}
                }
                export = export_menu(ui, false, can_export, compact);
                if let Some(analyze) = explain_menu(ui, busy, compact) {
                    explain = Some((one.clone(), analyze));
                }
                // "Repeat every": the last query again on a timer (reads only).
                repeat_menu(ui, repeat, last_reads, compact);
                if let Some(r) = file_menu(ui, s.cfg.read_only, has_file) {
                    file_menu_action = Some(r);
                }
                let format = theme::tool(icon::TEXT_ALIGN_LEFT, "Format", compact, t.muted_foreground, t.foreground);
                if ui
                    .button(format)
                    .on_hover_text("Format · Shift+Alt+F · formats the selection, if any")
                    .clicked()
                    || format_key
                {
                    format_in_place(text, selected_range, s.cfg.kind);
                }
                if !compact {
                    let used = ui.cursor().left() - start_x;
                    if full != Some(used) {
                        ui.data_mut(|d| d.insert_temp(variant, used));
                        if used + RIGHT_ROOM > avail {
                            ui.ctx().request_repaint();
                        }
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    detail_toggle(ui, cx.show_detail);
                    location_and_keys(ui, s, several);
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
                    let error_bytes = error_at.and_then(|(a, b)| {
                        let byte = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(i, _)| i);
                        (b <= text.chars().count()).then(|| byte(a)..byte(b))
                    });
                    let galley_cache = egui::Id::new(("editor_galley", tab.id));
                    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                        // Highlighting and laying out a long script costs more than a frame:
                        // the result is kept until anything it depends on changes.
                        let key = egui::Id::new((
                            buf.as_str(),
                            wrap.to_bits(),
                            &query,
                            case,
                            current_match,
                            error_bytes.clone(),
                            theme::is_light(),
                            ui.ctx().pixels_per_point().to_bits(),
                        ));
                        if let Some((k, g)) =
                            ui.data(|d| d.get_temp::<(egui::Id, std::sync::Arc<egui::Galley>)>(galley_cache))
                            && k == key
                        {
                            return g;
                        }
                        let mut job = syntax::highlight(buf.as_str(), &egui::FontId::monospace(13.0));
                        if let Some(r) = error_bytes.clone() {
                            syntax::mark_error(&mut job, r);
                        }
                        if !query.is_empty() {
                            let ranges = find::find_all(buf.as_str(), &query, case);
                            find::mark(&mut job, &ranges, current_match);
                        }
                        job.wrap.max_width = wrap;
                        let g = ui.fonts_mut(|f| f.layout_job(job));
                        ui.data_mut(|d| d.insert_temp(galley_cache, (key, g.clone())));
                        g
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
                            select_and_reveal(ui, &mut out, editor_id, a, b);
                        }
                    }
                    // Editing makes the error mark stale.
                    if out.response.changed() {
                        *error_at = None;
                    }
                    // A new error: the cursor goes to it, scrolled into view.
                    if error_jump && let Some((a, _)) = *error_at {
                        select_and_reveal(ui, &mut out, editor_id, a, a);
                        ui.memory_mut(|m| m.request_focus(editor_id));
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
                    // A name under the pointer: a table shows its columns on hover; Ctrl+click opens
                    // the table's data, or the code of a view, function, trigger or sequence, or
                    // the structure of the table a column belongs to. A name with several matches
                    // (overloads, a column in two tables) lists them to pick from.
                    let hovered = out.response.hover_pos().and_then(|p| {
                        let at = usize::from(out.galley.cursor_from_pos(p - out.galley_pos).index);
                        let (a, b) = sql::word_at(text, at)?;
                        let word: String = text.chars().skip(a).take(b - a).collect();
                        let statement: String = sql::statement_in(&ranges, at)
                            .map(|(a, b)| text.chars().skip(a).take(b - a).collect())
                            .unwrap_or_default();
                        let catalog = definition::Catalog {
                            tables: &s.tables,
                            objects: &s.objects,
                            columns: &s.schema_cols,
                            current_schema: &s.current_schema,
                        };
                        let found = definition::resolve(&catalog, &word, &statement);
                        (!found.is_empty()).then_some((p, found))
                    });
                    let picker_id = egui::Id::new(("definition_picker", tab.id));
                    if let Some((pointer, found)) = hovered {
                        let ctrl = ui.input(|i| i.modifiers.command);
                        if ctrl {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if ctrl && out.response.clicked() {
                            match found.as_slice() {
                                [one] => go_to = Some(definition_request(one)),
                                _ => {
                                    // Read before `data_mut`: the context can't be asked while its
                                    // memory is locked.
                                    let pass = ui.ctx().cumulative_pass_nr();
                                    ui.data_mut(|d| d.insert_temp(picker_id, (pointer, found.clone(), pass)));
                                }
                            }
                        }
                        let tip = out.response.response.clone();
                        match found.as_slice() {
                            [definition::Target::Table { schema, name }] => {
                                if let Some(t) = s.tables.iter().find(|t| &t.schema == schema && &t.name == name) {
                                    tip.on_hover_ui_at_pointer(|ui| table_tooltip(ui, s, t));
                                }
                            }
                            [one] => {
                                let what = one.describe();
                                tip.on_hover_ui_at_pointer(|ui| {
                                    ui.label(egui::RichText::new(&what).color(t.foreground));
                                    ui.label(
                                        egui::RichText::new("Ctrl+click opens the definition")
                                            .size(11.0)
                                            .color(t.subtle),
                                    );
                                });
                            }
                            many => {
                                let n = many.len();
                                tip.on_hover_ui_at_pointer(|ui| {
                                    for m in many.iter().take(TOOLTIP_COLUMNS) {
                                        ui.label(egui::RichText::new(m.describe()).color(t.foreground));
                                    }
                                    ui.label(
                                        egui::RichText::new(format!("{n} matches: Ctrl+click to choose"))
                                            .size(11.0)
                                            .color(t.subtle),
                                    );
                                });
                            }
                        }
                    }
                    if let Some(picked) = definition_picker(ui.ctx(), picker_id) {
                        go_to = Some(definition_request(&picked));
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
                        if can_preview {
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
    if go_to.is_some() {
        cx.request = go_to;
    }
    // "Repeat every": stops at the first error, so a broken query doesn't keep failing.
    let repeat_value = repeat_value.filter(|_| tab.error.is_none());
    let last_run = tab.editor_mut().and_then(|ed| {
        ed.repeat = repeat_value;
        ed.last_run
    });
    if let Some(every) = repeat_value
        && !tab.busy()
    {
        let wait = std::time::Duration::from_secs(every);
        match last_run.map(|at| at.elapsed()) {
            Some(passed) if passed < wait => ui.ctx().request_repaint_after(wait - passed),
            _ => app::rerun(tab, s),
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
    if let Some(choice) = export {
        do_export(ui, tab, s, choice);
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
    let pending_edits = app::editable_ref(&tab.kind).is_some_and(|ts| !ts.changes.is_empty());
    if save && pending_edits {
        if let Some(ts) = app::editable_mut(&mut tab.kind) {
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
        && let Some(ts) = app::editable_mut(&mut tab.kind)
    {
        ts.changes.pop();
    }
    changes_bar(ui, tab);
    confirm_save(ui.ctx(), tab, s);
    if let Some(plan) = tab.editor().and_then(|ed| ed.plan.as_ref()) {
        if let Some(e) = &tab.error {
            error_banner(ui, e);
        }
        let hints = index_suggestions(s, plan);
        let baseline = tab.editor().and_then(|ed| ed.pinned_plan.as_ref());
        match plan_view::plan_view(ui, plan, baseline, &hints) {
            Some(plan_view::PlanAction::Close) => {
                if let Some(ed) = tab.editor_mut() {
                    ed.plan = None;
                }
            }
            Some(plan_view::PlanAction::OpenSql(sql)) => cx.request = Some(Request::Sql(sql)),
            Some(plan_view::PlanAction::Pin) => {
                if let Some(ed) = tab.editor_mut() {
                    ed.pinned_plan = ed.plan.clone();
                }
            }
            Some(plan_view::PlanAction::Unpin) => {
                if let Some(ed) = tab.editor_mut() {
                    ed.pinned_plan = None;
                }
            }
            None => {}
        }
        return;
    }
    result_area(ui, tab, cx, false);
}

/// "Explain": `Some(analyze)` when a plan was asked for.
fn explain_menu(ui: &mut egui::Ui, busy: bool, compact: bool) -> Option<bool> {
    let t = theme::tokens();
    let mut pick = None;
    let label = theme::tool(
        icon::TREE_VIEW,
        "Explain",
        compact,
        t.muted_foreground,
        t.foreground,
    );
    ui.menu_button(label, |ui| {
        if ui
            .add_enabled(!busy, egui::Button::new("Estimated plan").shortcut_text("Ctrl+E"))
            .on_hover_text("EXPLAIN: how the database plans to run it, without running it")
            .clicked()
        {
            pick = Some(false);
            ui.close();
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Actual plan (ANALYZE)"))
            .on_hover_text("EXPLAIN ANALYZE: runs the query and times each step (reads only)")
            .clicked()
        {
            pick = Some(true);
            ui.close();
        }
    });
    pick
}

/// "Repeat every": runs the last query again every `repeat` seconds. Only offered once a
/// reading query ran (`enabled`), or to switch it off.
fn repeat_menu(ui: &mut egui::Ui, repeat: &mut Option<u64>, enabled: bool, compact: bool) {
    let t = theme::tokens();
    let label: egui::WidgetText = match *repeat {
        Some(n) => theme::icon_text(icon::ARROWS_CLOCKWISE, &format!("every {n} s"), t.lime, t.lime).into(),
        None if compact => theme::icon(icon::ARROWS_CLOCKWISE)
            .color(t.muted_foreground)
            .into(),
        None => theme::icon_text(icon::ARROWS_CLOCKWISE, "Repeat", t.muted_foreground, t.foreground).into(),
    };
    ui.add_enabled_ui(enabled || repeat.is_some(), |ui| {
        ui.menu_button(label, |ui| {
            ui.label(
                egui::RichText::new("Runs the last query again on its own")
                    .size(11.0)
                    .color(t.subtle),
            );
            for (n, label) in [
                (None, "Off"),
                (Some(2), "2 s"),
                (Some(5), "5 s"),
                (Some(10), "10 s"),
                (Some(30), "30 s"),
                (Some(60), "1 min"),
            ] {
                if ui.selectable_label(*repeat == n, label).clicked() {
                    *repeat = n;
                    ui.close();
                }
            }
        })
        .response
        .on_disabled_hover_text("Run a read query first (SELECT…)");
    });
}

/// The folder menu: open, run or save `.sql` files.
fn file_menu(ui: &mut egui::Ui, read_only: bool, has_file: bool) -> Option<Request> {
    let t = theme::tokens();
    let mut pick = None;
    ui.menu_button(theme::icon(icon::FOLDER_OPEN).color(t.muted_foreground), |ui| {
        if ui
            .add(egui::Button::new("Open file…").shortcut_text("Ctrl+O"))
            .clicked()
        {
            pick = Some(Request::OpenFile);
            ui.close();
        }
        if !read_only
            && ui
                .button("Run file…")
                .on_hover_text("Runs a large .sql statement by statement, without opening it in the editor")
                .clicked()
        {
            pick = Some(Request::RunFile);
            ui.close();
        }
        ui.separator();
        if ui
            .add(egui::Button::new("Save").shortcut_text("Ctrl+S"))
            .clicked()
        {
            pick = Some(Request::SaveFile(false));
            ui.close();
        }
        if ui
            .add(egui::Button::new("Save as…").shortcut_text("Ctrl+Shift+S"))
            .clicked()
        {
            pick = Some(Request::SaveFile(true));
            ui.close();
        }
    })
    .response
    .on_hover_text(if has_file {
        "This tab's .sql file"
    } else {
        "Open or save as a .sql file"
    });
    pick
}

/// Right end of the toolbar: where unqualified names go (so an UPDATE doesn't land somewhere
/// unexpected) and the run keys. Laid out right to left.
fn location_and_keys(ui: &mut egui::Ui, s: &Session, several: bool) {
    let t = theme::tokens();
    let here = match s.cfg.kind {
        crate::db::Kind::Postgres if !s.current_schema.is_empty() => {
            format!("on {} · {}", s.current_db, s.current_schema)
        }
        _ if !s.current_db.is_empty() => format!("on {}", s.current_db),
        _ => String::new(),
    };
    // Cut with "…" when there is little room, never drawn over the buttons.
    ui.add(
        egui::Label::new(
            egui::RichText::new(&here)
                .size(11.0)
                .monospace()
                .color(t.muted_foreground),
        )
        .truncate(),
    )
    .on_hover_text(format!(
        "{here} · database and schema for unprefixed names (change them in the sidebar)"
    ));
    let keys = if several {
        "Ctrl+Enter statement · F5 all"
    } else {
        "Ctrl+Enter / F5"
    };
    if ui.available_width() > 160.0 {
        ui.add(egui::Label::new(egui::RichText::new(keys).size(11.0).color(t.subtle)).truncate());
    }
}

/// Selects characters `a..b` in the editor (a cursor when equal) and scrolls them into view.
fn select_and_reveal(
    ui: &egui::Ui,
    out: &mut egui::text_edit::TextEditOutput,
    id: egui::Id,
    a: usize,
    b: usize,
) {
    let (start, end) = (egui::text::CCursor::new(a), egui::text::CCursor::new(b));
    out.state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(start, end)));
    out.state.clone().store(ui.ctx(), id);
    let rect = out.galley.pos_from_cursor(start);
    ui.scroll_to_rect(rect.translate(out.galley_pos.to_vec2()), Some(Align::Center));
}

/// `sql::statement_ranges` of the editor text, kept while the text stays the same.
fn statement_ranges_cached(ctx: &egui::Context, tab: u64, text: &str) -> Vec<(usize, usize)> {
    let cache = egui::Id::new(("statement_ranges", tab));
    let key = egui::Id::new(text);
    if let Some((k, r)) = ctx.data(|d| d.get_temp::<(egui::Id, Vec<(usize, usize)>)>(cache))
        && k == key
    {
        return r;
    }
    let r = sql::statement_ranges(text, true);
    ctx.data_mut(|d| d.insert_temp(cache, (key, r.clone())));
    r
}

/// `app::previewable` of what Ctrl+Enter would run, kept while that text stays the same: it
/// reads the whole text, which costs milliseconds on a big paste, and the toolbar asks every
/// frame.
fn previewable_cached(ctx: &egui::Context, tab: u64, sql: &str) -> bool {
    let cache = egui::Id::new(("previewable", tab));
    let key = egui::Id::new(sql);
    if let Some((k, v)) = ctx.data(|d| d.get_temp::<(egui::Id, bool)>(cache))
        && k == key
    {
        return v;
    }
    let v = app::previewable(sql);
    ctx.data_mut(|d| d.insert_temp(cache, (key, v)));
    v
}

/// What opening a definition target asks the app for.
fn definition_request(target: &definition::Target) -> Request {
    match target {
        definition::Target::Table { schema, name } => Request::Data(schema.clone(), name.clone()),
        definition::Target::Object(o) => Request::Object(o.clone()),
        definition::Target::Column {
            schema,
            table,
            column,
        } => Request::StructureColumn(schema.clone(), table.clone(), column.clone()),
    }
}

/// The list a Ctrl+click on an ambiguous name opened, at the pointer; the chosen target once
/// one is clicked. Escape or a click elsewhere closes it.
fn definition_picker(ctx: &egui::Context, id: egui::Id) -> Option<definition::Target> {
    let (at, found, opened): (egui::Pos2, Vec<definition::Target>, u64) = ctx.data(|d| d.get_temp(id))?;
    let t = theme::tokens();
    let mut picked = None;
    let area = egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(at + egui::vec2(4.0, 12.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(egui::RichText::new("Open which one?").size(11.0).color(t.subtle));
                for target in &found {
                    let b = egui::Button::new(egui::RichText::new(target.describe()).color(t.foreground))
                        .frame(false);
                    if ui.add(b).clicked() {
                        picked = Some(target.clone());
                    }
                }
            });
        });
    let escape = ctx.input(|i| i.key_pressed(Key::Escape));
    // The click that opened it is "elsewhere" too, in the frame it opened.
    let away = area.response.clicked_elsewhere() && ctx.cumulative_pass_nr() != opened;
    if picked.is_some() || escape || away {
        ctx.data_mut(|d| d.remove::<(egui::Pos2, Vec<definition::Target>, u64)>(id));
    }
    picked
}

/// Hover card for a table name in the editor: its columns and types.
pub(super) fn table_tooltip(ui: &mut egui::Ui, s: &Session, t: &crate::db::TableRef) {
    let tk = theme::tokens();
    ui.horizontal(|ui| {
        ui.label(theme::icon(if t.is_view { icon::EYE } else { icon::TABLE }).color(tk.muted_foreground));
        ui.label(egui::RichText::new(format!("{}.{}", t.schema, t.name)).color(tk.foreground));
        if let Some(n) = t.estimate {
            ui.label(
                egui::RichText::new(format!("~{} rows", palette::format_count(n)))
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
            egui::RichText::new(format!("… {} more", cols.len() - TOOLTIP_COLUMNS))
                .size(11.0)
                .color(tk.subtle),
        );
    }
    ui.label(
        egui::RichText::new("Ctrl+click opens the table")
            .size(11.0)
            .color(tk.subtle),
    );
}

/// Columns listed in the editor's hover card.
pub(super) const TOOLTIP_COLUMNS: usize = 16;

/// Tables smaller than this aren't worth an index suggestion: reading them whole is cheap.
pub(super) const INDEX_HINT_ROWS: i64 = 10_000;

/// `CREATE INDEX` suggestions for a plan: only for known tables of some size, and only with
/// columns that really are in them.
pub(super) fn index_suggestions(s: &Session, plan: &crate::plan::Plan) -> Vec<(String, String)> {
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
pub(super) fn request_run(tab: &mut Tab, s: &Session, sql: String, mode: RunMode) {
    let names = sql::param_names(&sql);
    if names.is_empty() {
        run_now(tab, s, sql, mode);
    } else if let Some(ed) = tab.editor_mut() {
        ed.params = Some(app::ParamPrompt {
            sql,
            names,
            mode,
            focused: false,
        });
    }
}

pub(super) fn run_now(tab: &mut Tab, s: &Session, sql: String, mode: RunMode) {
    match mode {
        RunMode::Explain(analyze) => return app::explain(tab, s, &sql, analyze),
        RunMode::Preview => return app::preview(tab, s, sql),
        // Connections set to "preview before changing" look first.
        RunMode::Run if s.cfg.preview_writes && app::previewable(&sql) && !s.cfg.read_only => {
            return app::preview(tab, s, sql);
        }
        RunMode::Run | RunMode::RunPreviewed => {}
    }
    let risk = sql::script_risk(&sql);
    if s.cfg.read_only && risk.writes {
        tab.error = Some("Read-only connection: only queries (SELECT, SHOW, EXPLAIN…) can run.".into());
    } else if !risk.dangers.is_empty() || (s.cfg.confirm_writes && risk.writes) {
        tab.confirm_run = Some(sql);
    } else {
        app::run_sql(tab, s, sql);
    }
}

/// "Parameters": one field per `:name`, remembered per tab for the next run.
pub(super) fn params_prompt(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let id = tab.id;
    let Some(ed) = tab.editor_mut() else { return };
    let Some(p) = &mut ed.params else { return };
    let t = theme::tokens();
    let (mut go, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new(("params", id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(420.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::BRACKETS_CURLY).size(16.0).color(t.lime));
                ui.label(egui::RichText::new("Parameters").size(15.0).color(t.foreground));
            });
            ui.label(
                egui::RichText::new("Numbers and NULL go in as they are; anything else becomes quoted text.")
                    .size(11.0)
                    .color(t.subtle),
            );
            ui.add_space(8.0);
            egui::Grid::new(("params_grid", id))
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    for (i, name) in p.names.iter().enumerate() {
                        ui.label(
                            egui::RichText::new(format!(":{name}"))
                                .monospace()
                                .color(t.number),
                        );
                        let v = ed.param_values.entry(name.clone()).or_default();
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
                    RunMode::Explain(_) => "Explain",
                    RunMode::Preview => "Preview",
                    RunMode::Run | RunMode::RunPreviewed => "Run",
                };
                if ui.add(theme::primary_button(Some(icon::PLAY), label)).clicked() {
                    go = true;
                }
                if ui.add(theme::outline_button("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
    if go && let Some(p) = ed.params.take() {
        let values = &ed.param_values;
        let sql = sql::bind_params(s.cfg.kind, &p.sql, |n| values.get(n).cloned().unwrap_or_default());
        run_now(tab, s, sql, p.mode);
    } else if cancel || modal.should_close() {
        ed.params = None;
    }
}

/// "Preview": how many rows an UPDATE/DELETE would touch and some of them; run it from here or
/// give up.
pub(super) fn preview_dialog(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let Some(p) = tab.editor().and_then(|ed| ed.preview.as_ref()) else {
        return;
    };
    let t = theme::tokens();
    let delete = sql::statement_words_first(&p.sql).is_some_and(|w| w == "DELETE");
    let (mut run, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new(("preview", tab.id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(720.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::EYE).size(16.0).color(t.lime));
                ui.label(egui::RichText::new("Preview").size(15.0).color(t.foreground));
                ui.label(
                    egui::RichText::new("nothing was changed")
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
                    let job = syntax::highlight(&p.sql, &egui::FontId::monospace(12.0));
                    ui.add(egui::Label::new(job).wrap());
                });
            ui.add_space(8.0);
            match &p.data {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Counting…").color(t.muted_foreground));
                    });
                }
                Some(Err(e)) => {
                    ui.label(egui::RichText::new(e).color(t.destructive));
                }
                Some(Ok(d)) => {
                    let verb = if delete { "deleted" } else { "changed" };
                    let color = if d.count == 0 {
                        t.muted_foreground
                    } else if delete {
                        t.destructive
                    } else {
                        t.warning
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "{} row(s) would be {verb}",
                            palette::format_count(d.count as i64)
                        ))
                        .size(18.0)
                        .color(color),
                    );
                    if !d.sample.rows.is_empty() {
                        let what = if d.after {
                            "as they would be (the statement ran in a rolled-back transaction)"
                        } else {
                            "as they are now"
                        };
                        let shown = d.sample.rows.len();
                        ui.label(
                            egui::RichText::new(format!("first {shown}, {what}"))
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
                let label = if delete { "Delete" } else { "Run" };
                let button = if delete {
                    danger_button(label)
                } else {
                    theme::primary_button(Some(icon::PLAY), label)
                };
                if ui.add_enabled(ready && !s.cfg.read_only, button).clicked() {
                    run = true;
                }
                if ui.add(theme::outline_button("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
    let close = run || cancel || modal.should_close();
    let p = if close {
        tab.editor_mut().and_then(|ed| ed.preview.take())
    } else {
        None
    };
    if run && let Some(p) = p {
        run_now(tab, s, p.sql, RunMode::RunPreviewed);
    }
}

/// A small read-only table for dialogs.
pub(super) fn preview_rows(ui: &mut egui::Ui, tab_id: u64, res: &QueryResult) {
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
pub(super) fn format_in_place(text: &mut String, selected: Option<(usize, usize)>, kind: crate::db::Kind) {
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
pub(super) fn confirm_run(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
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
                let title = if risk.dangers.is_empty() {
                    "Confirm changes"
                } else {
                    "Destructive statement"
                };
                ui.label(egui::RichText::new(title).size(15.0).color(t.foreground));
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("On {}:", s.cfg.name)).color(t.muted_foreground));
            for d in &risk.dangers {
                ui.label(
                    egui::RichText::new(format!("• {d}"))
                        .monospace()
                        .size(12.0)
                        .color(t.destructive),
                );
            }
            ui.add_space(4.0);
            let note = if risk.dangers.is_empty() {
                format!(
                    "This connection asks before changing data: {} statement(s) will run.",
                    sql::statement_ranges(&sql, false).len()
                )
            } else {
                "This may delete or change a lot of data and can't be undone.".to_string()
            };
            ui.label(egui::RichText::new(note).color(t.muted_foreground));
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if risk.dangers.is_empty() {
                    "Run"
                } else {
                    "Run anyway"
                };
                go = ui.add(danger_button(label)).clicked();
                cancel = ui.add(theme::outline_button("Cancel")).clicked();
            });
        });
    if go {
        tab.confirm_run = None;
        app::run_sql(tab, s, sql);
    } else if cancel || modal.should_close() {
        tab.confirm_run = None;
    }
}

pub(super) enum FavAction {
    Load(String),
    Save,
    Delete(String),
}

/// "★ Favorites" dropdown: save the current query, or load / delete a saved one.
pub(super) fn favorites_menu(
    ui: &mut egui::Ui,
    favs: &[Favorite],
    conn: &str,
    can_save: bool,
    compact: bool,
) -> Option<FavAction> {
    let t = theme::tokens();
    let mut action = None;
    let label = theme::tool(icon::STAR, "Favorites", compact, t.warning, t.foreground);
    ui.menu_button(label, |ui| {
        ui.set_width(380.0);
        let save = theme::icon_text(icon::PLUS, "Save current query…", t.lime, t.foreground);
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
            ui.label(egui::RichText::new("No saved queries").color(t.subtle));
        }
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for f in mine {
                let scope = if f.conn.is_empty() { "all connections" } else { "" };
                let r = ui.add(
                    egui::Button::new(egui::RichText::new(&f.name).color(t.foreground))
                        .frame_when_inactive(false)
                        .truncate()
                        .min_size(egui::vec2(ui.available_width(), 22.0))
                        .right_text(egui::RichText::new(scope).size(11.0).color(t.subtle)),
                );
                r.context_menu(|ui| {
                    if ui.button("Delete").clicked() {
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
            egui::RichText::new("Right-click an item to delete it")
                .size(10.0)
                .color(t.subtle),
        );
    });
    action
}

/// "History" dropdown with this connection's recent queries; returns the one picked.
pub(super) enum HistoryPick {
    Load(String),
    All,
}

pub(super) fn history_menu(
    ui: &mut egui::Ui,
    history: &[Entry],
    conn: &str,
    compact: bool,
) -> Option<HistoryPick> {
    let t = theme::tokens();
    let mut picked = None;
    let label = theme::tool(
        icon::CLOCK_COUNTER_CLOCKWISE,
        "History",
        compact,
        t.muted_foreground,
        t.foreground,
    );
    ui.menu_button(label, |ui| {
        ui.set_width(460.0);
        let now = history::now();
        let mine: Vec<&Entry> = history.iter().rev().filter(|e| e.conn == conn).take(40).collect();
        let all = theme::icon_text(
            icon::LIST_MAGNIFYING_GLASS,
            "View full history…",
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
            ui.label(egui::RichText::new("No queries yet").color(t.subtle));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::fake_session;
    use crate::db::Kind;

    /// "Confirm before any change" asks for every writing statement, and only for those.
    #[test]
    fn a_connection_that_confirms_asks_before_writes() {
        let (mut s, _jobs, _replies) = fake_session(Kind::Postgres);
        let mut tab = Tab::new(
            1,
            "t".into(),
            TabKind::Sql(Box::new(app::Editor::new(String::new()))),
        );
        let write = "UPDATE t SET a = 1 WHERE id = 2";
        run_now(&mut tab, &s, write.into(), RunMode::Run);
        assert!(
            tab.confirm_run.is_none(),
            "off by default: a WHERE keeps it quiet"
        );
        s.cfg.confirm_writes = true;
        run_now(&mut tab, &s, "SELECT 1".into(), RunMode::Run);
        assert!(tab.confirm_run.is_none(), "reads never ask");
        run_now(&mut tab, &s, write.into(), RunMode::Run);
        assert_eq!(tab.confirm_run.as_deref(), Some(write));
    }
}
