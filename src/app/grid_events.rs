//! What the result grid asks for (`GridEvent`), carried out on the tab.

use std::ops::ControlFlow;

use super::*;

/// Carries out what the grid asked for. Returns text to put on the clipboard, if any.
pub fn handle_grid_event(tab: &mut Tab, s: &Session, event: GridEvent, editable: bool) -> Option<Effect> {
    if let ControlFlow::Break(effect) = any_result_event(tab, s, &event, editable) {
        return effect;
    }
    if editable {
        edit_event(tab, s, event);
    }
    None
}

/// Things that work on any result, editable or not. `Continue` leaves the event to `edit_event`.
fn any_result_event(
    tab: &mut Tab,
    s: &Session,
    event: &GridEvent,
    editable: bool,
) -> ControlFlow<Option<Effect>> {
    let k = s.cfg.kind;
    let Some(res) = &tab.result else {
        return ControlFlow::Break(None);
    };
    let no_cols = Vec::new();
    let cols = editable_ref(&tab.kind).map_or(&no_cols, |ts| &ts.cols);
    let effect = match event {
        GridEvent::Select(row, col) => {
            tab.selected = Some((*row, *col));
            tab.anchor = None;
            tab.detail_hidden = None;
            if let TabKind::Table(ts) = &mut tab.kind {
                ts.option_pick = None;
            }
            None
        }
        GridEvent::Copy(row, how) => {
            let table = match &tab.kind {
                TabKind::Table(ts) => k.qualified(&ts.schema, &ts.table),
                // An editable editor result knows its table too.
                TabKind::Sql(ed) if let Some(src) = &ed.source => k.qualified(&src.schema, &src.table),
                _ => "table".to_string(),
            };
            Some(Effect::Copy(match how {
                CopyAs::Json => values::row_json(res, cols, *row),
                CopyAs::Insert => sql::insert_row(k, &table, &res.columns, res.rows.row(*row)),
            }))
        }
        // A foreign key is chosen from the referenced rows instead of typed.
        GridEvent::StartEdit(row, col)
            if editable
                && let Some(fk) = cols
                    .iter()
                    .find(|c| c.name == res.columns[*col])
                    .and_then(|c| c.references.clone()) =>
        {
            let (row, col) = (*row, *col);
            let column = res.columns[col].clone();
            let current = res.rows.cell(row, col).map(str::to_owned);
            tab.selected = Some((row, col));
            open_fk_picker(tab, s, PickFor::Cell(row, col), &column, fk, current);
            None
        }
        GridEvent::OpenValue(row, col) => {
            open_value_editor(tab, *row, *col, editable);
            None
        }
        GridEvent::Extend(row, col) => {
            tab.anchor = tab.anchor.or(tab.selected).or(Some((*row, *col)));
            tab.selected = Some((*row, *col));
            None
        }
        GridEvent::SelectAll => {
            let view = tab.view();
            if let (false, Some(&first), Some(&last)) = (res.rows.is_empty(), view.first(), view.last()) {
                tab.anchor = Some((0, first));
                tab.selected = Some((res.rows.len() - 1, last));
            }
            None
        }
        GridEvent::ToggleColumn(col) => {
            let name = res.columns[*col].clone();
            if let Some(i) = tab.hidden_cols.iter().position(|n| *n == name) {
                tab.hidden_cols.remove(i);
            } else {
                tab.hidden_cols.push(name);
                // A selection on a column that just went away would be invisible.
                if tab.selected.is_some_and(|(_, c)| c == *col) {
                    tab.selected = None;
                    tab.anchor = None;
                }
            }
            None
        }
        GridEvent::ShowAllColumns => {
            tab.hidden_cols.clear();
            None
        }
        GridEvent::TogglePin(col) => {
            let name = res.columns[*col].clone();
            if let Some(i) = tab.pinned_cols.iter().position(|n| *n == name) {
                tab.pinned_cols.remove(i);
            } else {
                tab.pinned_cols.push(name);
            }
            None
        }
        GridEvent::FilterValues(col) => {
            let name = res.columns[*col].clone();
            open_value_filter(tab, s, &name);
            None
        }
        GridEvent::ColumnStats(col) => {
            start_column_stats(tab, s, *col);
            None
        }
        GridEvent::CopyMarkdown => {
            if res.rows.is_empty() {
                return ControlFlow::Break(None);
            }
            let range = tab.range().unwrap_or(CellRange {
                r0: 0,
                r1: res.rows.len() - 1,
                cols: tab.view(),
            });
            let kinds = values::column_kinds(res, cols);
            let overlay = editable_ref(&tab.kind).and_then(|ts| ts.overlay(res));
            let shown = overlay.as_ref().map_or(res, |o| &o.result);
            Some(Effect::Copy(values::to_markdown(shown, &range, &kinds)))
        }
        GridEvent::CopyInList(col) => {
            if res.rows.is_empty() {
                return ControlFlow::Break(None);
            }
            // The selected rows when the selection covers this column, else every row.
            let rows = match tab.selection() {
                Some(r) if r.cols.contains(col) => r.r0..=r.r1,
                _ => 0..=res.rows.len() - 1,
            };
            let overlay = editable_ref(&tab.kind).and_then(|ts| ts.overlay(res));
            let shown = overlay.as_ref().map_or(res, |o| &o.result);
            let kind = values::column_kinds(shown, cols)[*col];
            let (text, n) = values::to_in_list(k, shown, *col, rows, kind);
            if n == 0 {
                return ControlFlow::Break(None);
            }
            Some(Effect::Copy(text))
        }
        GridEvent::CopyCells { headers } => {
            let Some(range) = tab.selection() else {
                return ControlFlow::Break(None);
            };
            // Copy what the grid shows, pending edits included.
            let overlay = editable_ref(&tab.kind).and_then(|ts| ts.overlay(res));
            let shown = overlay.as_ref().map_or(res, |o| &o.result);
            Some(Effect::Copy(values::to_tsv(shown, &range, *headers)))
        }
        GridEvent::FollowIncoming(row, i) => follow_incoming(tab, k, *row, *i),
        GridEvent::FollowFk(row, col) => {
            let info = cols.iter().find(|c| c.name == res.columns[*col]);
            let target = info.and_then(|c| c.references.as_ref());
            let value = res.rows.cell(*row, *col);
            target.zip(value).map(|(target, value)| Effect::OpenFiltered {
                schema: target.schema.clone(),
                table: target.table.clone(),
                filter: sql::filter_condition(k, &target.column, FilterOp::Eq, Some(value)),
            })
        }
        GridEvent::ColumnWidths(widths) => {
            let TabKind::Table(ts) = &tab.kind else {
                return ControlFlow::Break(None);
            };
            let (schema, table) = (ts.schema.clone(), ts.table.clone());
            tab.col_widths
                .extend(widths.iter().map(|(name, w)| (name.clone(), w.round())));
            Some(Effect::ColumnWidths {
                schema,
                table,
                widths: widths.clone(),
            })
        }
        // Filtering doesn't change data, so read-only tables get it too.
        GridEvent::Filter(row, col, op) => {
            let cond = sql::filter_condition(k, &res.columns[*col], *op, res.rows.cell(*row, *col));
            if let TabKind::Table(ts) = &mut tab.kind {
                ts.filter = sql::and(&ts.filter, &cond);
                ts.page = 0;
                reload(tab, s);
            }
            None
        }
        _ => return ControlFlow::Continue(()),
    };
    ControlFlow::Break(effect)
}

/// The full-value editor (long text, JSON) for one cell.
fn open_value_editor(tab: &mut Tab, row: usize, col: usize, editable: bool) {
    let Some(res) = &tab.result else { return };
    let column = res.columns[col].clone();
    let info = editable_ref(&tab.kind).and_then(|ts| ts.cols.iter().find(|c| c.name == column));
    let original = res.rows.cell(row, col).map(str::to_owned);
    let text = original.clone().unwrap_or_default();
    let data_type = info.map_or(String::new(), |c| c.data_type.clone());
    let json_column = data_type.starts_with("json");
    let is_json = values::looks_like_json(&text);
    // An expression of an editor's SELECT isn't a column that can be written.
    let editable = editable && info.is_some();
    tab.selected = Some((row, col));
    tab.value_editor = Some(ValueEditor {
        row,
        col,
        editable,
        json_column,
        compact: is_json && !text.contains('\n'),
        text: if is_json { values::json_pretty(&text) } else { text },
        null: original.is_none(),
        original,
        column,
        data_type,
    });
}

/// Tables: the database counts every row that passes the filter. Editor results: only what
/// was loaded.
fn start_column_stats(tab: &mut Tab, s: &Session, col: usize) {
    let k = s.cfg.kind;
    let Some(res) = &tab.result else { return };
    let name = res.columns[col].clone();
    let no_cols = Vec::new();
    let cols = editable_ref(&tab.kind).map_or(&no_cols, |ts| &ts.cols);
    let kind = values::column_kinds(res, cols)[col];
    match &tab.kind {
        TabKind::Table(ts) => {
            let filter = ts.effective_filter(k);
            let ordered = matches!(kind, ValueKind::Number | ValueKind::Date);
            let (summary, top) = sql::column_stats(k, &ts.schema, &ts.table, &name, &filter, ordered);
            let scope = if filter.is_empty() {
                "whole table"
            } else {
                "rows matching the filter"
            };
            tab.col_stats = Some(StatsView {
                column: name,
                scope: scope.to_string(),
                data: None,
            });
            tab.begin_job();
            s.send(Job::ColumnStats {
                tab: tab.id,
                summary,
                top,
            });
        }
        _ => {
            let data = values::local_column_stats(res, col, kind);
            tab.col_stats = Some(StatsView {
                column: name,
                scope: format!(
                    "{} loaded {}",
                    res.rows.len(),
                    crate::values::noun(res.rows.len() as u64, "row")
                ),
                data: Some(Ok(data)),
            });
        }
    }
}

/// Opens the rows of another table that point at this row.
fn follow_incoming(tab: &Tab, k: Kind, row: usize, i: usize) -> Option<Effect> {
    let (TabKind::Table(ts), Some(res)) = (&tab.kind, &tab.result) else {
        return None;
    };
    let fk = ts.incoming.get(i)?;
    let col = res.columns.iter().position(|c| *c == fk.target)?;
    let value = res.rows.cell(row, col)?;
    Some(Effect::OpenFiltered {
        schema: fk.schema.clone(),
        table: fk.table.clone(),
        filter: sql::filter_condition(k, &fk.column, FilterOp::Eq, Some(value)),
    })
}

/// Edits on an editable result: sorting, cell edits, paste/fill, row deletes.
fn edit_event(tab: &mut Tab, s: &Session, event: GridEvent) {
    let k = s.cfg.kind;
    let selection = tab.selection();
    let view = tab.view();
    let is_table = matches!(tab.kind, TabKind::Table(_));
    let Some(res) = &tab.result else { return };
    let Some(ts) = editable_mut(&mut tab.kind) else {
        return;
    };
    let kinds = values::column_kinds(res, &ts.cols);
    match event {
        // Editor results keep the order their query asked for.
        GridEvent::Sort(_) | GridEvent::SortAlso(_) if !is_table => {}
        // Expressions and aliases in an editor's SELECT aren't columns of the table.
        GridEvent::StartEdit(_, col) if !ts.cols.iter().any(|c| c.name == res.columns[col]) => {
            tab.error = Some(format!(
                "\"{}\" is not a column of {}: it can't be edited",
                res.columns[col], ts.table
            ));
        }
        GridEvent::Sort(col) => {
            // A plain click sorts by this column alone: ascending, descending, then off.
            ts.order = match ts.order.as_slice() {
                [(c, true)] if *c == col => vec![(col, false)],
                [(c, false)] if *c == col => Vec::new(),
                _ => vec![(col, true)],
            };
            ts.page = 0;
            reload(tab, s);
        }
        GridEvent::SortAlso(col) => {
            match ts.order.iter().position(|(c, _)| *c == col) {
                // Each sort column goes ascending, descending, then out of the sort.
                Some(i) if ts.order[i].1 => ts.order[i].1 = false,
                Some(i) => {
                    ts.order.remove(i);
                }
                None => ts.order.push((col, true)),
            }
            ts.page = 0;
            reload(tab, s);
        }
        // Booleans open a true/false/NULL dropdown and ENUMs one with their values; nothing is
        // written until a value is picked.
        GridEvent::StartEdit(row, col)
            if kinds[col] == ValueKind::Bool || !enum_values(&ts.cols, res, col).is_empty() =>
        {
            tab.selected = Some((row, col));
            ts.option_pick = Some((row, col));
        }
        GridEvent::StartEdit(row, col) => {
            let text = res.rows.cell(row, col).map(str::to_owned).unwrap_or_default();
            tab.selected = Some((row, col));
            ts.edit = Some(Edit {
                row,
                col,
                original: text.clone(),
                text,
                focused: false,
            });
        }
        GridEvent::CancelEdit => {
            ts.edit = None;
            ts.option_pick = None;
        }
        GridEvent::Commit(row, col, val) => {
            ts.edit = None;
            ts.option_pick = None;
            if let Err(e) = set_cell(ts, res, &kinds, k, (row, col), val) {
                tab.error = Some(e);
            }
        }
        GridEvent::Paste(text) => {
            let Some(sel) = selection else { return };
            match paste(ts, res, &kinds, k, &sel, &view, &text) {
                Ok(p) => {
                    if let Some((anchor, selected)) = p.selected {
                        tab.anchor = Some(anchor);
                        tab.selected = Some(selected);
                    }
                    tab.error = (p.skipped > 0).then(|| {
                        format!(
                            "{} pasted {} outside the page and {} ignored.",
                            p.skipped,
                            if p.skipped == 1 { "cell fell" } else { "cells fell" },
                            if p.skipped == 1 { "was" } else { "were" }
                        )
                    });
                }
                Err(e) => tab.error = Some(e),
            }
        }
        // Toggles: deleting an already-deleted row restores it.
        GridEvent::DeleteRow(row) => {
            let key = ts.row_key(res, row);
            let before = ts.changes.len();
            ts.changes
                .retain(|c| !matches!(c, Change::Delete { key: k2 } if *k2 == key));
            if ts.changes.len() == before {
                ts.changes.push(Change::Delete { key });
            }
        }
        GridEvent::DuplicateRow(row) => open_duplicate_form(tab, row),
        GridEvent::AskFill => tab.fill_prompt = Some(String::new()),
        GridEvent::AskBulk(col) if is_table => {
            tab.bulk_prompt = Some(BulkPrompt {
                column: res.columns[col].clone(),
                expression: String::new(),
                selected_only: selection
                    .as_ref()
                    .is_some_and(|s| s.r1 > s.r0 || s.cols.len() == 1),
            });
        }
        GridEvent::AskBulk(_) => {}
        GridEvent::FillSelection(value) => {
            let Some(sel) = &selection else { return };
            tab.fill_prompt = None;
            let value = &value;
            let cells = (sel.r0..=sel.r1)
                .flat_map(|row| sel.cols.iter().map(move |&col| ((row, col), value.clone())));
            if let Err(e) = set_cells(ts, res, &kinds, k, cells) {
                tab.error = Some(e);
            }
        }
        GridEvent::FillDown => {
            let Some(sel) = &selection else { return };
            // The first row as the grid shows it, pending edits included.
            let overlay = ts.overlay(res);
            let shown = overlay.as_ref().map_or(res, |o| &o.result);
            let top: Vec<Option<String>> = sel
                .cols
                .iter()
                .map(|&c| shown.rows.cell(sel.r0, c).map(str::to_owned))
                .collect();
            let cells = (sel.r0 + 1..=sel.r1).flat_map(|row| {
                sel.cols
                    .iter()
                    .zip(&top)
                    .map(move |(&col, v)| ((row, col), v.clone()))
            });
            if let Err(e) = set_cells(ts, res, &kinds, k, cells) {
                tab.error = Some(e);
            }
        }
        GridEvent::DeleteRows => {
            let Some(sel) = &selection else { return };
            for row in sel.r0..=sel.r1 {
                let key = ts.row_key(res, row);
                if !ts
                    .changes
                    .iter()
                    .any(|c| matches!(c, Change::Delete { key: k2 } if *k2 == key))
                {
                    ts.changes.push(Change::Delete { key });
                }
            }
        }
        // Handled by `any_result_event`.
        GridEvent::Select(..)
        | GridEvent::CopyMarkdown
        | GridEvent::CopyInList(..)
        | GridEvent::ColumnWidths(..)
        | GridEvent::FollowIncoming(..)
        | GridEvent::ColumnStats(..)
        | GridEvent::FilterValues(..)
        | GridEvent::OpenValue(..)
        | GridEvent::ToggleColumn(..)
        | GridEvent::ShowAllColumns
        | GridEvent::TogglePin(..)
        | GridEvent::Extend(..)
        | GridEvent::SelectAll
        | GridEvent::CopyCells { .. }
        | GridEvent::Copy(..)
        | GridEvent::Filter(..)
        | GridEvent::FollowFk(..) => {}
    }
}

/// Sets several cells, stopping at the first one that can't be set.
fn set_cells(
    ts: &mut TableState,
    res: &QueryResult,
    kinds: &[ValueKind],
    k: Kind,
    cells: impl IntoIterator<Item = ((usize, usize), Option<String>)>,
) -> Result<(), String> {
    cells
        .into_iter()
        .try_for_each(|(at, val)| set_cell(ts, res, kinds, k, at, val))
}

struct Pasted {
    /// New (anchor, selected) covering what was pasted.
    selected: Option<((usize, usize), (usize, usize))>,
    /// Cells that fell outside the page.
    skipped: usize,
}

/// Fills from the selection's top-left corner; whatever falls outside the page is left out.
/// Columns go rightwards in on-screen order from the selection's first one.
fn paste(
    ts: &mut TableState,
    res: &QueryResult,
    kinds: &[ValueKind],
    k: Kind,
    sel: &CellRange,
    view: &[usize],
    text: &str,
) -> Result<Pasted, String> {
    let top = sel.r0;
    let start = view.iter().position(|&c| c == sel.cols[0]).unwrap_or(0);
    let cells = values::parse_tsv(text);
    let mut skipped = 0;
    for (dr, line) in cells.iter().enumerate() {
        for (dc, v) in line.iter().enumerate() {
            let row = top + dr;
            let Some(&col) = view.get(start + dc).filter(|_| row < res.rows.len()) else {
                skipped += 1;
                continue;
            };
            // Spreadsheets have no NULL: the word itself stands for it, as the grid shows it.
            let val = (v != "NULL").then(|| v.clone());
            set_cell(ts, res, kinds, k, (row, col), val)?;
        }
    }
    let rows = cells.len().min(res.rows.len().saturating_sub(top));
    let cols = cells
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .min(view.len().saturating_sub(start));
    let selected =
        (rows > 0 && cols > 0).then(|| ((top, view[start]), (top + rows - 1, view[start + cols - 1])));
    Ok(Pasted { selected, skipped })
}

/// The values of the ENUM column shown in result column `col` (empty when it isn't one).
pub fn enum_values<'a>(cols: &'a [ColumnInfo], res: &QueryResult, col: usize) -> &'a [String] {
    cols.iter()
        .find(|c| c.name == res.columns[col])
        .map_or(&[], |c| &c.enum_values)
}
