//! The table browser tab: filter bar, pending changes, insert form and CSV import.

use super::*;
use crate::ui::theme::A11y;

/// Narrowest the WHERE filter gets before the toolbar turns compact.
const FILTER_MIN: f32 = 260.0;

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
    let can_export = !busy && tab.result.as_ref().is_some_and(|r| !r.columns.is_empty());
    let mut export = None;
    let mut show_structure = false;
    let mut import_csv = false;
    let TabKind::Table(ts) = &mut tab.kind else {
        return;
    };
    // A view has rows to read, not to insert into (most refuse it): no "Record" or import.
    let is_view = s.is_view(&ts.schema, &ts.table);
    let mut view_source = false;

    let t = theme::tokens();
    if pager(ui, ts, status, rows, busy, s, tab_id) {
        reload_now = true;
    }

    let toolbar = egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            // Roomier than the rest of the UI: the filter is where you type the most.
            ui.spacing_mut().interact_size.y = 28.0;
            ui.horizontal(|ui| {
                // Short of room: a narrower search box and icon-only buttons, so the filter keeps
                // a usable width (decided from the full buttons' last measured width).
                let full_id = egui::Id::new(("toolbar_right_full", tab_id));
                let full_right = ui.data(|d| d.get_temp::<f32>(full_id)).unwrap_or(440.0);
                let compact = ui.available_width() - 200.0 - 90.0 - full_right < FILTER_MIN;
                // Search: the term in any column, on top of the WHERE filter.
                let search = ui.add(
                    TextEdit::singleline(&mut ts.search)
                        .id(search_id)
                        .hint_text(if compact {
                            "Find  (Ctrl+F)"
                        } else {
                            "Find in all columns  (Ctrl+F)"
                        })
                        .margin(egui::Margin::symmetric(8, 6))
                        .desired_width(if compact { 130.0 } else { 200.0 }),
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
                        .hint_text("id > 10 AND name LIKE 'a%'   (Enter applies)")
                        .font(egui::FontId::monospace(13.0))
                        .margin(egui::Margin::symmetric(8, 6))
                        .layouter(&mut syntax::sql_line_layouter(13.0))
                        .desired_width(w),
                );
                r.clone()
                    .on_hover_text_at_pointer(egui::RichText::new(&ts.filter).monospace());
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    ts.page = 0;
                    reload_now = true;
                }
                let b = toolbar_buttons(
                    ui,
                    is_view,
                    s.cfg.read_only,
                    compact,
                    can_export,
                    cx.show_detail,
                    tab_id,
                );
                view_source = b.view_source;
                add_record = b.add_record;
                reload_now |= b.reload;
                export = b.export;
                import_csv = b.import;
                show_structure = b.structure;
            })
        });
    if show_structure {
        cx.request = Some(Request::Structure(ts.schema.clone(), ts.table.clone()));
    }
    let r = toolbar.response.rect;
    ui.painter()
        .hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, t.border));
    if let Some(choice) = export {
        do_export(ui, tab, s, choice);
    }
    // A .csv or spreadsheet dropped on the window opens the import dialog with it.
    let dropped = ui.ctx().input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path())
            .find(|p| {
                p.extension().is_some_and(|e| e.eq_ignore_ascii_case("csv"))
                    || crate::import::is_spreadsheet(p)
            })
            .map(|p| p.display().to_string())
    });
    if view_source && let TabKind::Table(ts) = &tab.kind {
        cx.request = Some(Request::ViewSource(ts.schema.clone(), ts.table.clone()));
    }
    let is_view = matches!(&tab.kind, TabKind::Table(ts) if s.is_view(&ts.schema, &ts.table));
    if (import_csv || dropped.is_some()) && !s.cfg.read_only && !is_view {
        app::open_import(tab, dropped);
    }
    if add_record {
        app::open_insert_form(tab);
    } else if reload_now {
        app::reload(tab, s);
    }
    export_banner(ui, tab);
    changes_bar(ui, tab);
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
        Some(InsertAction::PickFk(i)) => {
            let f = &fields[i];
            if let Some(fk) = f.references.clone() {
                let (column, current) = (f.name.clone(), Some(f.text.clone()).filter(|t| !t.is_empty()));
                app::open_fk_picker(tab, s, app::PickFor::InsertField(i), &column, fk, current);
            }
        }
        None => {}
    }
    // The picker opened from the form fills the field.
    if let Some((app::PickFor::InsertField(i), key)) = fk_dialog(ctx, tab, s)
        && let TabKind::Table(ts) = &mut tab.kind
        && let Some(f) = ts.insert.as_mut().and_then(|fields| fields.get_mut(i))
    {
        f.mode = app::FieldMode::Value;
        f.text = key;
    }
}

/// "N pending changes · Undo · Discard · Save" while there are unsaved edits.
pub(super) fn changes_bar(ui: &mut egui::Ui, tab: &mut Tab) {
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
                    parts.push(format!("{edits} {}", crate::values::noun(edits as u64, "edit")));
                }
                if deletes > 0 {
                    parts.push(format!(
                        "{deletes} {}",
                        crate::values::noun(deletes as u64, "deletion")
                    ));
                }
                ui.label(
                    egui::RichText::new(format!("{} pending, not saved yet", parts.join(" and ")))
                        .color(t.foreground),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    save = ui
                        .add(theme::primary_button(Some(icon::FLOPPY_DISK), "Save"))
                        .on_hover_text("Ctrl+S")
                        .clicked();
                    discard = ui.add(theme::outline_button("Discard")).clicked();
                    undo = ui
                        .button(theme::icon_text(
                            icon::ARROW_COUNTER_CLOCKWISE,
                            "Undo",
                            t.muted_foreground,
                            t.foreground,
                        ))
                        .on_hover_text("Ctrl+Z")
                        .clicked();
                });
            });
        });
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
    let modal = theme::modal(egui::Id::new(("confirm_save", tab.id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(520.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::FLOPPY_DISK).size(16.0).color(if deletes > 0 {
                    t.destructive
                } else {
                    t.accent_text
                }));
                ui.label(
                    egui::RichText::new(format!(
                        "Save {} {}?",
                        stmts.len(),
                        crate::values::noun(stmts.len() as u64, "change")
                    ))
                    .size(15.0)
                    .color(t.foreground),
                );
            });
            ui.add_space(4.0);
            ui.label(
            egui::RichText::new(format!(
                "In {}.{}: {edits} {}, {deletes} {}. All in one transaction: if one fails, nothing is saved.",
                ts.schema,
                ts.table,
                crate::values::noun(edits as u64, "edit"),
                crate::values::noun(deletes as u64, "deletion")
            ))
            .color(t.muted_foreground),
        );
            ui.add_space(8.0);
            egui::Frame::new()
                .fill(t.sidebar)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for st in &stmts {
                                let job = syntax::highlight(
                                    &format!("{};", st.render(k)),
                                    &egui::FontId::monospace(12.0),
                                );
                                ui.add(egui::Label::new(job).wrap());
                            }
                        });
                });
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                go = if deletes > 0 {
                    ui.add(danger_button("Save and delete")).clicked()
                } else {
                    ui.add(theme::primary_button(Some(icon::FLOPPY_DISK), "Save"))
                        .clicked()
                };
                cancel = ui.add(theme::outline_button("Back")).clicked();
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
        t.accent_text
    };
    let mut action = None;
    let name_id = egui::Id::new(("filter_name", tab_id));
    ui.menu_button(theme::icon(icon::FUNNEL).size(15.0).color(color), |ui| {
        ui.set_width(340.0);
        ui.label(egui::RichText::new("SAVED FILTERS").size(12.0).color(t.subtle));
        if mine.is_empty() {
            ui.label(egui::RichText::new("No saved filters for this table").color(t.subtle));
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
                        .tip("Delete filter")
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
                        "Name of the current filter"
                    } else {
                        "Write a WHERE to save it"
                    })
                    .desired_width(220.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let ok = can_save && !name.trim().is_empty();
            if (ui.add_enabled(ok, egui::Button::new("Save")).clicked() || (enter && ok)) && action.is_none()
            {
                action = Some(FilterAction::Save(name.trim().to_string()));
                name.clear();
                ui.close();
            }
        });
        ui.data_mut(|d| d.insert_temp(name_id, name));
    })
    .response
    .tip("Saved filters");
    action
}

/// The pager at the bottom: status, the page's rows of the total, previous and next. Returns
/// whether the page changed (to reload).
fn pager(
    ui: &mut egui::Ui,
    ts: &mut app::TableState,
    status: String,
    rows: usize,
    busy: bool,
    s: &Session,
    tab_id: u64,
) -> bool {
    let t = theme::tokens();
    let full = rows >= page_size();
    let mut reload_now = false;
    let bar = egui::Frame::new()
        .fill(t.sidebar)
        .inner_margin(egui::Margin::symmetric(8, 2));
    egui::Panel::bottom(egui::Id::new(("pager", tab_id)))
        .frame(bar)
        .show(ui, |ui| {
            ui.spacing_mut().interact_size.y = 20.0;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(status).small().color(t.muted_foreground));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let next = egui::Button::new(theme::icon(icon::CARET_RIGHT)).frame(false);
                    if ui.add_enabled(full, next).tip("Next page").clicked() {
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
                        (Some(n), _) => format!(" of {}", palette::format_count(n)),
                        (None, Some(n)) if ts.filter.trim().is_empty() => {
                            format!(" of ~{}", palette::format_count(n))
                        }
                        _ => String::new(),
                    };
                    ui.label(
                        egui::RichText::new(format!("{range}{of}"))
                            .small()
                            .color(t.muted_foreground),
                    );
                    let prev = egui::Button::new(theme::icon(icon::CARET_LEFT)).frame(false);
                    if ui.add_enabled(ts.page > 0, prev).tip("Previous page").clicked() {
                        ts.page -= 1;
                        reload_now = true;
                    }
                    if busy {
                        busy_indicator(ui, s, tab_id);
                    }
                });
            });
        });
    reload_now
}

/// What the table toolbar's buttons were clicked for.
#[derive(Default)]
struct ToolbarButtons {
    add_record: bool,
    view_source: bool,
    reload: bool,
    export: Option<ExportChoice>,
    import: bool,
    structure: bool,
}

/// The right end of the table toolbar: Record (or View code), reload, detail panel, export,
/// import and structure. Remembers the width it took, for the filter to fit next frame.
fn toolbar_buttons(
    ui: &mut egui::Ui,
    is_view: bool,
    read_only: bool,
    compact: bool,
    can_export: bool,
    show_detail: &mut bool,
    tab_id: u64,
) -> ToolbarButtons {
    let t = theme::tokens();
    let right_id = egui::Id::new(("toolbar_right", tab_id));
    let full_id = egui::Id::new(("toolbar_right_full", tab_id));
    let mut b = ToolbarButtons::default();
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let start = ui.cursor().right();
        if is_view {
            let code = theme::tool(icon::CODE, "View code", compact, t.muted_foreground, t.foreground);
            b.view_source = ui
                .button(code)
                .named("View code")
                .on_hover_text("The view's definition, in the editor")
                .clicked();
        } else {
            b.add_record = ui
                .add_enabled(
                    !read_only,
                    theme::primary_button(Some(icon::PLUS), if compact { "" } else { "Record" }),
                )
                .tip("New record")
                .on_disabled_hover_text("Read-only connection")
                .clicked();
        }
        if ui
            .button(theme::icon(icon::ARROWS_CLOCKWISE))
            .tip("Reload (F5)")
            .clicked()
        {
            b.reload = true;
        }
        detail_toggle(ui, show_detail);
        b.export = export_menu(ui, true, can_export, compact);
        b.import = !is_view
            && ui
                .add_enabled(
                    !read_only,
                    egui::Button::new(theme::icon(icon::UPLOAD_SIMPLE).color(t.muted_foreground)),
                )
                .tip("Import CSV or Excel (or drag the file onto the window)")
                .on_disabled_hover_text("Read-only connection")
                .clicked();
        let structure = theme::tool(
            icon::TREE_STRUCTURE,
            "Structure",
            compact,
            t.muted_foreground,
            t.foreground,
        );
        b.structure = ui
            .button(structure)
            .named("Structure")
            .on_hover_text("Structure · columns, indexes and DDL")
            .clicked();
        let used = start - ui.cursor().right();
        ui.data_mut(|d| d.insert_temp(right_id, used));
        if !compact {
            ui.data_mut(|d| d.insert_temp(full_id, used));
        }
    });
    b
}
