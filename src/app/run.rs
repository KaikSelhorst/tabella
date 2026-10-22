//! Running statements from a tab: reload, exec, previews, load more, explain.

use super::*;

/// Jobs at least this long announce themselves when they finish in the background.
pub(super) const LONG_JOB: std::time::Duration = std::time::Duration::from_secs(10);

/// Copies the settings that don't need a new connection (name, color, preview) from an edited
/// copy of the open connection. Returns false when other changes wait for a reconnect.
pub fn apply_live_settings(s: &mut Session, edited: &ConnConfig) -> bool {
    s.cfg.name = edited.name.clone();
    s.cfg.color = edited.color;
    s.cfg.preview_writes = edited.preview_writes;
    s.cfg.confirm_writes = edited.confirm_writes;
    s.cfg.group = edited.group.clone();
    // The saved default; the session's own mode is switched in the "…" menu.
    s.cfg.manual_commit = edited.manual_commit;
    s.cfg.same_session_settings(edited)
}

/// Re-runs the current page query of a table tab.
pub fn reload(tab: &mut Tab, s: &Session) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    if !ts.changes.is_empty() {
        // A reload (page, sort, filter) would show rows the pending changes don't refer to.
        tab.error = Some(format!(
            "Save (Ctrl+S) or discard the {} pending {} before reloading.",
            ts.changes.len(),
            crate::values::noun(ts.changes.len() as u64, "change")
        ));
        return;
    }
    if let Err(e) = sql::check_filter(s.cfg.kind, &ts.filter) {
        tab.error = Some(e);
        return;
    }
    tab.selected = None;
    ts.edit = None;
    let sql = ts.page_sql(s.cfg.kind);
    // Queued after the page so rows show up first.
    let needs_count = ts.counted_filter.as_deref() != Some(ts.effective_filter(s.cfg.kind).as_str());
    let id = tab.id;
    tab.send_rows_job(s, Job::Query { tab: id, sql });
    if needs_count && let TabKind::Table(ts) = &mut tab.kind {
        request_count(id, ts, s);
    }
}

pub(super) fn request_count(tab: u64, ts: &mut TableState, s: &Session) {
    let filter = ts.effective_filter(s.cfg.kind);
    ts.counted_filter = Some(filter.clone());
    ts.total = None;
    if ts.estimate.is_none_or(|n| n < EXACT_COUNT_LIMIT) {
        s.send(Job::Count {
            tab,
            sql: sql::count(s.cfg.kind, &ts.schema, &ts.table, &filter),
        });
    }
}

/// Runs a write statement on the table, then refreshes the current page. Row edits and deletes
/// must touch exactly one row; otherwise the user is told the row changed elsewhere.
pub fn exec(tab: &mut Tab, s: &Session, stmt: Stmt, expect_one: bool) {
    let TabKind::Table(ts) = &mut tab.kind else { return };
    if let Err(e) = sql::check_filter(s.cfg.kind, &ts.filter) {
        tab.error = Some(e);
        return;
    }
    let refresh = ts.page_sql(s.cfg.kind);
    // Inserts and deletes change the total.
    request_count(tab.id, ts, s);
    let id = tab.id;
    tab.send_rows_job(
        s,
        Job::Exec {
            tab: id,
            stmt,
            refresh,
            expect_one,
        },
    );
}

/// Starts a query from an SQL editor tab; its result is recorded in the history.
pub fn run_sql(tab: &mut Tab, s: &Session, sql: String) {
    if let Some(ed) = tab.editor_mut() {
        if let Some(src) = &ed.source
            && !src.changes.is_empty()
        {
            let n = src.changes.len();
            tab.error = Some(format!(
                "Save (Ctrl+S) or discard the {n} pending {} in the result before running another query.",
                crate::values::noun(n as u64, "change")
            ));
            return;
        }
        ed.source = None;
        ed.plan = None;
        ed.error_at = None;
        ed.set_last_query(sql.clone());
        ed.last_run = Some(Instant::now());
    }
    tab.pending_sql = Some(sql.clone());
    tab.error = None;
    let id = tab.id;
    tab.send_rows_job(s, Job::Query { tab: id, sql });
}

/// Where an error is in the editor.
#[derive(Debug, PartialEq)]
pub struct ErrorSpot {
    /// Char range in the editor text: the word the server complains about.
    pub start: usize,
    pub end: usize,
    /// 1-based, for the message.
    pub line: usize,
    pub column: usize,
}

/// Finds the error of `sent` in the editor `text`, when the editor still has that SQL (a run
/// with `:parameters` sent other text, so it has no place there).
pub fn locate_error(text: &str, sent: &str, e: &crate::error::DbError) -> Option<ErrorSpot> {
    let at = text.find(sent)? + e.position_in(sent)?;
    let ident = |c: char| c.is_alphanumeric() || matches!(c, '_' | '"' | '`' | '.');
    let rest = &text[at..];
    let len = match rest.chars().next()? {
        c if ident(c) => rest.find(|c: char| !ident(c)).unwrap_or(rest.len()),
        c => c.len_utf8(),
    };
    let before = &text[..at];
    let start = before.chars().count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    Some(ErrorSpot {
        start,
        end: start + text[at..at + len].chars().count(),
        line: before.matches('\n').count() + 1,
        column: text[line_start..at].chars().count() + 1,
    })
}

/// Whether `sql` is a single UPDATE or DELETE (what "Preview" works on).
pub fn previewable(sql: &str) -> bool {
    sql::statement_ranges(sql, false).len() == 1
        && sql::statement_words_first(sql).is_some_and(|w| w == "UPDATE" || w == "DELETE")
}

/// Starts a preview of an UPDATE/DELETE.
pub fn preview(tab: &mut Tab, s: &Session, sql: String) {
    let Some(ed) = tab.editor_mut() else { return };
    ed.preview = Some(PreviewState {
        sql: sql.clone(),
        data: None,
    });
    tab.error = None;
    tab.begin_job();
    s.send(Job::Preview { tab: tab.id, sql });
}

/// "Repeat every": runs the tab's last query again, quietly (no history entry, the
/// editable source and the selection stay).
pub fn rerun(tab: &mut Tab, s: &Session) {
    if has_unsaved(tab) {
        return;
    }
    let Some(ed) = tab.editor_mut() else { return };
    if ed.last_query.trim().is_empty() {
        return;
    }
    ed.last_run = Some(Instant::now());
    let sql = ed.last_query.clone();
    let id = tab.id;
    tab.send_rows_job(s, Job::Query { tab: id, sql });
}

/// A truncated result that "Load more" can fetch the rest of: one plain read, which is safe to
/// run again (a statement that writes, even with RETURNING, would repeat its effect).
pub fn can_load_more(tab: &Tab) -> bool {
    let Some(ed) = tab.editor() else { return false };
    !tab.busy()
        && ed.source.as_ref().is_none_or(|s| s.changes.is_empty())
        && tab.result.as_ref().is_some_and(|r| r.truncated)
        && ed.last_single
        && ed.last_reads
}

/// Runs the tab's last query again keeping another `row_limit` rows.
pub fn load_more(tab: &mut Tab, s: &Session) {
    if !can_load_more(tab) {
        return;
    }
    let Some(ed) = tab.editor() else { return };
    let sql = ed.last_query.clone();
    let limit = tab.result.as_ref().map_or(0, |r| r.rows.len()) + crate::db::max_rows();
    let id = tab.id;
    tab.send_rows_job(s, Job::MoreRows { tab: id, sql, limit });
}

/// Asks for the plan of one statement. ANALYZE runs it, so it is refused for anything but reads.
pub fn explain(tab: &mut Tab, s: &Session, sql: &str, analyze: bool) {
    let parts = sql::statement_ranges(sql, false);
    let [(a, b)] = parts[..] else {
        tab.error =
            Some("Explain works on one statement at a time: put the cursor on it or select it.".into());
        return;
    };
    let one: String = sql.chars().skip(a).take(b - a).collect();
    if analyze && sql::script_risk(&one).writes {
        tab.error =
            Some("EXPLAIN ANALYZE really runs the statement: here it's only used on read queries.".into());
        return;
    }
    tab.error = None;
    tab.begin_job();
    s.send(Job::Explain {
        tab: tab.id,
        sql: one,
        analyze,
    });
}
