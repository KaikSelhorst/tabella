//! Table tabs: value filters, imports, and cell edits waiting to be saved.

use super::*;

/// Opens "Filter values" for a column of a table tab, with what was unchecked last time.
pub fn open_value_filter(tab: &mut Tab, s: &Session, column: &str) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let data_type = ts
        .cols
        .iter()
        .find(|c| c.name == column)
        .map_or("", |c| c.data_type.as_str());
    let unchecked = ts
        .value_conds
        .iter()
        .find(|v| v.column == column)
        .map(|v| v.unchecked.clone())
        .unwrap_or_default();
    ts.value_filter = Some(ValueFilter {
        column: column.to_string(),
        expr: sql::value_expr(s.cfg.kind, column, data_type),
        values: None,
        complete: false,
        unchecked,
        search: String::new(),
        focused: false,
    });
    load_values(tab, s);
}

/// Asks for the open filter's values: the rows the other conditions let through (its own
/// condition left out, so what it hides can be checked again).
pub fn load_values(tab: &mut Tab, s: &Session) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let Some(vf) = &ts.value_filter else { return };
    let k = s.cfg.kind;
    // The WHERE box holds what is typed, applied or not.
    if let Err(e) = sql::check_filter(k, &ts.filter) {
        tab.error = Some(e);
        return;
    }
    let mut filter = ts.filter.clone();
    if let Some(own) = ts.value_conds.iter().find(|v| v.column == vf.column)
        && let Some(rest) = sql::remove_condition(&filter, &own.cond)
    {
        filter = rest;
    }
    let names: Vec<String> = ts.cols.iter().map(|c| c.name.clone()).collect();
    let filter = sql::and(&filter, &sql::search_condition(k, &names, &ts.search));
    let sql = sql::distinct_values(k, &ts.schema, &ts.table, &vf.expr, &filter, &vf.search);
    if let Some(vf) = &mut ts.value_filter {
        vf.values = None;
    }
    tab.begin_job();
    s.send(Job::Distinct { tab: tab.id, sql });
}

/// Puts the checked values in the table's filter (replacing what this column had) and reloads.
pub fn apply_value_filter(tab: &mut Tab, s: &Session) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let Some(vf) = ts.value_filter.take() else { return };
    let Some(Ok(values)) = &vf.values else { return };
    let (unchecked, checked): (Vec<_>, Vec<_>) = values
        .iter()
        .map(|(v, _)| v.clone())
        .partition(|v| vf.unchecked.contains(v));
    let cond = sql::values_condition(s.cfg.kind, &vf.expr, &checked, &unchecked, vf.complete);
    if let Some(i) = ts.value_conds.iter().position(|v| v.column == vf.column) {
        let old = &ts.value_conds[i].cond;
        match sql::remove_condition(&ts.filter, old) {
            Some(rest) => ts.filter = rest,
            // Still in the WHERE box but edited around: adding the new one would keep both.
            None if ts.filter.contains(old.trim()) => {
                tab.error = Some(format!(
                    "The WHERE box was edited around this column's condition: remove {old} \
                     there first."
                ));
                ts.value_filter = Some(vf);
                return;
            }
            // The user took it out.
            None => {}
        }
        ts.value_conds.remove(i);
    }
    if let Some(cond) = cond {
        ts.filter = sql::and(&ts.filter, &cond);
        ts.value_conds.push(ValueCond {
            column: vf.column,
            cond,
            unchecked: unchecked.into_iter().collect(),
        });
    }
    ts.page = 0;
    reload(tab, s);
}

/// Opens the CSV import dialog, with `path` already chosen when a file was dropped on the window.
pub fn open_import(tab: &mut Tab, path: Option<String>) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let mut form = ts.import.take().unwrap_or(ImportForm {
        has_header: true,
        empty_is_null: true,
        ..Default::default()
    });
    if let Some(p) = path {
        form.path = p;
    }
    ts.import = Some(form);
}

/// Re-reads the file preview when the path or the header option changed, and guesses the
/// mapping from the column names.
pub fn refresh_import_preview(ts: &mut TableState) {
    let Some(form) = &mut ts.import else { return };
    let path = form.path.trim().trim_matches('"').to_string();
    if path.is_empty() {
        form.preview = None;
        form.mapping.clear();
        return;
    }
    if form
        .preview
        .as_ref()
        .is_some_and(|(p, h, _)| *p == path && *h == form.has_header)
    {
        return;
    }
    let pv = import::preview(std::path::Path::new(&path), form.has_header);
    let names: Vec<String> = ts.cols.iter().map(|c| c.name.clone()).collect();
    form.mapping = match &pv {
        Ok(p) => import::auto_mapping(&p.headers, &names),
        Err(_) => Vec::new(),
    };
    form.preview = Some((path, form.has_header, pv));
}

/// Starts the import as set up in the dialog.
pub fn submit_import(tab: &mut Tab, s: &Session) {
    if s.cfg.read_only {
        tab.error = Some("Read-only connection: importing isn't possible.".into());
        return;
    }
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let Some(form) = &ts.import else { return };
    let Some((path, _, Ok(pv))) = &form.preview else {
        return;
    };
    if form.mapping.iter().all(Option::is_none) {
        tab.error = Some("Map at least one file column to a table column.".into());
        return;
    }
    let cols_of = |kind| {
        ts.cols
            .iter()
            .filter(|c| values::value_kind(Some(&c.data_type), std::iter::empty()) == kind)
            .map(|c| c.name.clone())
            .collect()
    };
    let (bool_cols, number_cols) = (cols_of(ValueKind::Bool), cols_of(ValueKind::Number));
    let spec = import::ImportSpec {
        path: path.into(),
        kind: s.cfg.kind,
        schema: ts.schema.clone(),
        table: ts.table.clone(),
        delimiter: pv.delimiter,
        has_header: form.has_header,
        mapping: form.mapping.clone(),
        bool_cols,
        number_cols,
        empty_is_null: form.empty_is_null,
    };
    tab.error = None;
    tab.begin_job();
    s.send(Job::Import {
        tab: tab.id,
        spec: Box::new(spec),
    });
}

/// Adds `column = value` (or IS NULL) to a table tab's filter and reloads.
pub fn filter_by_value(tab: &mut Tab, s: &Session, column: &str, value: Option<&str>) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    let cond = sql::filter_condition(s.cfg.kind, column, FilterOp::Eq, value);
    ts.filter = sql::and(&ts.filter, &cond);
    ts.page = 0;
    reload(tab, s);
}

/// An ENUM column only takes one of its values: the server would refuse anything else only when
/// saving, so a typo or a pasted value is caught here instead. MySQL compares enum values
/// ignoring case, so it gets the spelling the type declares.
pub(super) fn enum_value(k: Kind, column: &str, values: &[String], v: &str) -> Result<String, String> {
    let found = values
        .iter()
        .find(|e| e.as_str() == v || (k == Kind::MySql && e.eq_ignore_ascii_case(v)));
    match found {
        Some(e) => Ok(e.clone()),
        None => {
            let list: Vec<&str> = values.iter().take(8).map(String::as_str).collect();
            let more = if values.len() > 8 { ", …" } else { "" };
            Err(format!(
                "\"{v}\" is not a value of {column}: {}{more}",
                list.join(", ")
            ))
        }
    }
}

/// Records a new value for a cell as a pending change (saved together with Ctrl+S). Typed
/// booleans ("true", "yes", "0"...) are normalized for the engine.
pub(super) fn set_cell(
    ts: &mut TableState,
    res: &QueryResult,
    kinds: &[ValueKind],
    k: Kind,
    (row, col): (usize, usize),
    val: Option<String>,
) -> Result<(), String> {
    if !ts.cols.iter().any(|c| c.name == res.columns[col]) {
        return Err(format!(
            "\"{}\" is not a column of {}: it can't be edited",
            res.columns[col], ts.table
        ));
    }
    let val = match (kinds[col], val) {
        (ValueKind::Bool, Some(v)) => match sql::parse_bool(&v) {
            Some(b) => Some(sql::bool_value(k, b).to_string()),
            None => return Err(format!("\"{v}\" is not a boolean (use true or false)")),
        },
        (_, v) => v,
    };
    let val = match (val, ts.cols.iter().find(|c| c.name == res.columns[col])) {
        (Some(v), Some(c)) if !c.enum_values.is_empty() => Some(enum_value(k, &c.name, &c.enum_values, &v)?),
        (v, _) => v,
    };
    let key = ts.row_key(res, row);
    let col_name = res.columns[col].clone();
    let before = res.rows.cell(row, col).map(str::to_owned);
    ts.changes
        .retain(|c| !matches!(c, Change::Set { key: k2, col: c2, .. } if *k2 == key && *c2 == col_name));
    let same = match kinds[col] {
        ValueKind::Bool => {
            before.as_deref().and_then(sql::parse_bool) == val.as_deref().and_then(sql::parse_bool)
        }
        _ => before == val,
    };
    if !same {
        ts.changes.push(Change::Set {
            key,
            col: col_name,
            value: val,
            before,
        });
    }
    Ok(())
}

/// The table state behind a tab's grid edits: a table tab's own, or the source table of an
/// editable editor result (once its columns are known).
/// The table whose rows the grid edits: a table tab's, or the one an editor's SELECT read from
/// (once its columns arrived).
pub fn editable_mut(kind: &mut TabKind) -> Option<&mut TableState> {
    match kind {
        TabKind::Table(ts) => Some(ts),
        TabKind::Sql(ed) => ed.source.as_deref_mut().filter(|s| !s.cols.is_empty()),
        _ => None,
    }
}

pub fn editable_ref(kind: &TabKind) -> Option<&TableState> {
    match kind {
        TabKind::Table(ts) => Some(ts),
        TabKind::Sql(ed) => ed.source.as_deref().filter(|s| !s.cols.is_empty()),
        _ => None,
    }
}

/// Undoes the most recent pending change.
pub fn undo_change(tab: &mut Tab) {
    if let Some(ts) = editable_mut(&mut tab.kind) {
        ts.changes.pop();
    }
}

pub fn discard_changes(tab: &mut Tab) {
    if let Some(ts) = editable_mut(&mut tab.kind) {
        ts.changes.clear();
        ts.confirm_save = false;
    }
}

/// Writes every pending change in one transaction, then reloads the page (or re-runs the
/// editor's SELECT).
pub fn save_changes(tab: &mut Tab, s: &Session) {
    let is_table = matches!(tab.kind, TabKind::Table(_));
    let query = tab.editor().map(|ed| ed.source_query.clone()).unwrap_or_default();
    let Some(ts) = editable_mut(&mut tab.kind) else {
        return;
    };
    ts.confirm_save = false;
    if ts.changes.is_empty() {
        return;
    }
    // The page read again after saving takes the WHERE box as it is now.
    if is_table && let Err(e) = sql::check_filter(s.cfg.kind, &ts.filter) {
        tab.error = Some(e);
        return;
    }
    let stmts = ts.change_statements(s.cfg.kind);
    let refresh = if is_table { ts.page_sql(s.cfg.kind) } else { query };
    let id = tab.id;
    tab.error = None;
    tab.send_rows_job(
        s,
        Job::Batch {
            tab: id,
            stmts,
            refresh,
        },
    );
}

pub fn has_unsaved(tab: &Tab) -> bool {
    editable_ref(&tab.kind).is_some_and(|ts| !ts.changes.is_empty())
}
