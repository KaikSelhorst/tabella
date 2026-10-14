//! The table browser tab: filter bar, pending changes, insert form and CSV import.

use super::*;

pub(super) fn table_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
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

pub(super) fn import_dialog(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
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

pub(super) fn insert_form(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
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
pub(super) fn changes_bar(ui: &mut egui::Ui, tab: &mut Tab, s: &Session) {
    let Some(ts) = app::editable_mut(&mut tab.kind) else {
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
    } else if save && let Some(ts) = app::editable_mut(&mut tab.kind) {
        ts.confirm_save = true;
    }
}

/// Summary before saving: what will change, and the exact SQL. Deletions make the confirm
/// button red; Enter never confirms here.
pub(super) fn confirm_save(ctx: &egui::Context, tab: &mut Tab, s: &Session) {
    let Some(ts) = app::editable_mut(&mut tab.kind) else {
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

pub(super) enum FilterAction {
    Apply(String),
    Save(String),
    Delete(String),
}

/// The funnel before WHERE: this table's saved filters, and saving the current one by name.
pub(super) fn filters_menu(
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
