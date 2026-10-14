//! The SQL editor tab: toolbar, editor, parameters, previews and confirmations before running.

use super::*;

pub(super) fn sql_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let busy = tab.busy();
    let can_export = !busy && tab.result.as_ref().is_some_and(|r| !r.columns.is_empty());
    let mut export = None;
    let TabKind::Sql(ed) = &mut tab.kind else {
        return;
    };
    let tab_last_query = ed.last_query.clone();
    let mut repeat_value = ed.repeat;
    let repeat = &mut repeat_value;
    let has_file = ed.file.is_some();
    let error_jump = std::mem::take(&mut ed.error_jump);
    let (text, error_at) = (&mut ed.text, &mut ed.error_at);
    let mut run: Option<String> = None;
    let mut explain: Option<(String, bool)> = None;
    let mut preview: Option<String> = None;
    let mut open_table: Option<(String, String)> = None;
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
                    let error_bytes = error_at.and_then(|(a, b)| {
                        let byte = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(i, _)| i);
                        (b <= text.chars().count()).then(|| byte(a)..byte(b))
                    });
                    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                        let mut job = syntax::highlight(buf.as_str(), egui::FontId::monospace(13.0));
                        if let Some(r) = error_bytes.clone() {
                            syntax::mark_error(&mut job, r);
                        }
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
                    // Editing makes the error mark stale.
                    if out.response.changed() {
                        *error_at = None;
                    }
                    // A new error: the cursor goes to it, scrolled into view.
                    if error_jump && let Some((a, _)) = *error_at {
                        let at = egui::text::CCursor::new(a);
                        out.state.cursor.set_char_range(Some(egui::text::CCursorRange::one(at)));
                        out.state.clone().store(ui.ctx(), editor_id);
                        ui.memory_mut(|m| m.request_focus(editor_id));
                        let rect = out.galley.pos_from_cursor(at);
                        ui.scroll_to_rect(rect.translate(out.galley_pos.to_vec2()), Some(Align::Center));
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
    // "Repetir a cada": stops at the first error, so a broken query doesn't keep failing.
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
    changes_bar(ui, tab, s);
    confirm_save(ui.ctx(), tab, s);
    if let Some(plan) = tab.editor().and_then(|ed| ed.plan.as_ref()) {
        if let Some(e) = &tab.error {
            error_banner(ui, e);
        }
        let hints = index_suggestions(s, plan);
        match plan_view::plan_view(ui, plan, &hints) {
            Some(plan_view::PlanAction::Close) => {
                if let Some(ed) = tab.editor_mut() {
                    ed.plan = None;
                }
            }
            Some(plan_view::PlanAction::OpenSql(sql)) => cx.request = Some(Request::Sql(sql)),
            None => {}
        }
        return;
    }
    result_area(ui, tab, cx, false);
}

/// The sidebar's table for a name typed in the editor (`clientes`, `public.clientes`,
/// `"Clientes"`), ignoring case when there's no exact match.
pub(super) fn table_named<'a>(s: &'a Session, word: &str) -> Option<&'a crate::db::TableRef> {
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
pub(super) fn table_tooltip(ui: &mut egui::Ui, s: &Session, t: &crate::db::TableRef) {
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
                ui.label(egui::RichText::new("Parâmetros").size(15.0).color(t.foreground));
            });
            ui.label(
                egui::RichText::new("Números e NULL entram como estão; o resto vira texto entre aspas.")
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
        let p = ed.params.take().expect("open prompt");
        let values = &ed.param_values;
        let sql = sql::bind_params(s.cfg.kind, &p.sql, |n| values.get(n).cloned().unwrap_or_default());
        run_now(tab, s, sql, p.mode);
    } else if cancel || modal.should_close() {
        ed.params = None;
    }
}

/// "Prévia": how many rows an UPDATE/DELETE would touch and some of them; run it from here or
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

pub(super) enum FavAction {
    Load(String),
    Save,
    Delete(String),
}

/// "★ Favoritos" dropdown: save the current query, or load / delete a saved one.
pub(super) fn favorites_menu(
    ui: &mut egui::Ui,
    favs: &[Favorite],
    conn: &str,
    can_save: bool,
) -> Option<FavAction> {
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
pub(super) enum HistoryPick {
    Load(String),
    All,
}

pub(super) fn history_menu(ui: &mut egui::Ui, history: &[Entry], conn: &str) -> Option<HistoryPick> {
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
