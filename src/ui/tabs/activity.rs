//! "Server activity": sessions, locks, slow queries and the server panel.

use super::*;

/// How often the activity tab refreshes itself while "Auto refresh" is on.
pub(super) const ACTIVITY_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Sessions on the server, with buttons to cancel the selected one's query or close it.
pub(super) fn activity_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let t = theme::tokens();
    let busy = tab.busy();
    // Session id of the selected row (first column), when it is a number.
    let target = tab
        .selected
        .and_then(|(row, _)| tab.result.as_ref()?.rows.get(row)?.first()??.parse::<i64>().ok());
    // The statement of the selected row ("slow queries"), to open in the editor.
    let selected_query = tab.selected.and_then(|(row, _)| {
        let res = tab.result.as_ref()?;
        let col = res.columns.iter().position(|c| c == "query")?;
        res.rows.get(row)?.get(col)?.map(str::to_owned)
    });
    // Where its unqualified tables are: the digest's own database (MySQL), else the current one.
    let selected_schema = tab
        .selected
        .and_then(|(row, _)| {
            let res = tab.result.as_ref()?;
            let col = res.columns.iter().position(|c| c == "db")?;
            res.rows
                .get(row)?
                .get(col)?
                .map(str::to_owned)
                .filter(|d| !d.is_empty())
        })
        .unwrap_or_else(|| match s.cfg.kind {
            crate::db::Kind::MySql => s.current_db.clone(),
            _ if s.current_schema.is_empty() => "public".into(),
            _ => s.current_schema.clone(),
        });
    let TabKind::Activity(st) = &mut tab.kind else {
        return;
    };
    let mut refresh = false;
    let mut switched = false;
    let mut reset_stats = false;
    let mut suggest = false;
    egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::PULSE).size(15.0).color(t.muted_foreground));
                ui.spacing_mut().item_spacing.x = 0.0;
                for (view, label, tip) in [
                    (
                        ActivityView::Sessions,
                        "Sessions",
                        "Who is connected and what they're running",
                    ),
                    (
                        ActivityView::Locks,
                        "Locks",
                        "Sessions waiting on another one's locks (the first column is the blocker)",
                    ),
                    (
                        ActivityView::Slow,
                        "Slow queries",
                        "What took the most server time since the statistics were last reset",
                    ),
                    (
                        ActivityView::Server,
                        "Server",
                        "Version, size, connections, cache, biggest tables and unused indexes",
                    ),
                ] {
                    let on = st.view == view;
                    let color = if on { t.foreground } else { t.muted_foreground };
                    if ui
                        .add(egui::Button::new(egui::RichText::new(label).color(color)).selected(on))
                        .on_hover_text(tip)
                        .clicked()
                        && !on
                    {
                        st.view = view;
                        switched = true;
                    }
                }
                ui.spacing_mut().item_spacing.x = 8.0;
                if busy {
                    ui.spinner();
                }
                let view = st.view;
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let refresh_btn = theme::icon_text(
                        icon::ARROWS_CLOCKWISE,
                        "Refresh",
                        t.muted_foreground,
                        t.foreground,
                    );
                    if ui.add_enabled(!busy, egui::Button::new(refresh_btn)).clicked() {
                        refresh = true;
                    }
                    if view == ActivityView::Server {
                        return;
                    }
                    ui.checkbox(&mut st.auto_refresh, "every 2 s");
                    ui.separator();
                    if view == ActivityView::Slow {
                        if ui
                            .add_enabled(
                                !busy,
                                egui::Button::new(theme::icon_text(
                                    icon::ERASER,
                                    "Reset statistics",
                                    t.muted_foreground,
                                    t.foreground,
                                )),
                            )
                            .on_hover_text(
                                "Restarts the count: useful to measure only what happens from now on",
                            )
                            .clicked()
                        {
                            reset_stats = true;
                        }
                        if ui
                            .add_enabled(
                                selected_query.is_some() && !busy,
                                egui::Button::new(theme::icon_text(
                                    icon::LIGHTBULB,
                                    "Suggest index",
                                    t.muted_foreground,
                                    t.foreground,
                                )),
                            )
                            .on_hover_text(
                                "Looks at the selected query's WHERE, JOIN and ORDER BY columns and the table's \
                                 indexes",
                            )
                            .on_disabled_hover_text("Select a query")
                            .clicked()
                        {
                            suggest = true;
                        }
                        if ui
                            .add_enabled(
                                selected_query.is_some(),
                                egui::Button::new(theme::icon_text(
                                    icon::CODE,
                                    "Open in editor",
                                    t.muted_foreground,
                                    t.foreground,
                                )),
                            )
                            .on_disabled_hover_text("Select a query")
                            .clicked()
                            && let Some(q) = &selected_query
                        {
                            cx.request = Some(Request::Sql(q.clone()));
                        }
                        return;
                    }
                    let end =
                        theme::icon_text(icon::PLUGS, "Kill connection", t.muted_foreground, t.foreground);
                    if ui
                        .add_enabled(target.is_some() && !busy, egui::Button::new(end))
                        .on_disabled_hover_text("Select a session")
                        .clicked()
                    {
                        st.confirm_kill = target.map(|id| (id, true));
                    }
                    let stop = theme::icon_text(icon::STOP, "Cancel query", t.muted_foreground, t.foreground);
                    if ui
                        .add_enabled(target.is_some() && !busy, egui::Button::new(stop))
                        .on_disabled_hover_text("Select a session")
                        .clicked()
                    {
                        st.confirm_kill = target.map(|id| (id, false));
                    }
                });
            });
        });
    if switched {
        tab.result = None;
        tab.selected = None;
        tab.anchor = None;
        tab.error = None;
        app::refresh_activity(tab, s);
    }
    // The server overview is a panel, refreshed by hand.
    if let TabKind::Activity(st) = &tab.kind
        && st.view == ActivityView::Server
    {
        if refresh && !switched {
            app::refresh_activity(tab, s);
        }
        if let TabKind::Activity(st) = &tab.kind {
            server_panel(ui, tab.id, st.server.as_ref(), tab.busy());
        }
        return;
    }
    if switched {
        result_area(ui, tab, cx, false);
        return;
    }
    if reset_stats {
        app::reset_query_stats(tab, s);
    }
    if suggest && let Some(q) = &selected_query {
        app::suggest_index(tab, s, q.clone(), selected_schema);
    }
    let TabKind::Activity(st) = &mut tab.kind else {
        return;
    };
    // While the confirmation is open the list stays still, so the selection can't move under it.
    if st.auto_refresh && st.confirm_kill.is_none() && st.advice.is_none() && !busy && !reset_stats {
        let due = st.last_refresh.is_none_or(|at| at.elapsed() >= ACTIVITY_EVERY);
        if due {
            refresh = true;
        } else if let Some(at) = st.last_refresh {
            ui.ctx()
                .request_repaint_after(ACTIVITY_EVERY.saturating_sub(at.elapsed()));
        }
    }
    if let Some((id, terminate)) = st.confirm_kill {
        match confirm_kill(ui.ctx(), tab.id, id, terminate) {
            Some(true) => app::kill_session(tab, s),
            Some(false) => {
                if let TabKind::Activity(st) = &mut tab.kind {
                    st.confirm_kill = None;
                }
            }
            None => {}
        }
    } else if refresh {
        app::refresh_activity(tab, s);
    }
    if let TabKind::Activity(st) = &tab.kind
        && let Some(e) = &st.kill_error
    {
        error_banner(ui, e);
    }
    if let TabKind::Activity(st) = &mut tab.kind
        && let Some(advice) = &st.advice
    {
        match index_advice(ui.ctx(), tab.id, advice) {
            Some(AdviceAction::Open(ddl)) => {
                st.advice = None;
                cx.request = Some(Request::Sql(ddl));
            }
            Some(AdviceAction::Close) => st.advice = None,
            None => {}
        }
    }
    result_area(ui, tab, cx, false);
}

enum AdviceAction {
    Open(String),
    Close,
}

/// The index "Suggest index" came up with (or why there is none): to copy or open in the
/// editor, never run from here.
fn index_advice(
    ctx: &egui::Context,
    tab_id: u64,
    advice: &Result<crate::advisor::Advice, String>,
) -> Option<AdviceAction> {
    let t = theme::tokens();
    let mut action = None;
    let modal = egui::Modal::new(egui::Id::new(("index_advice", tab_id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(520.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::LIGHTBULB).size(16.0).color(t.lime));
                ui.label(
                    egui::RichText::new("Suggest index")
                        .size(15.0)
                        .color(t.foreground),
                );
            });
            ui.add_space(6.0);
            match advice {
                Ok(a) => {
                    ui.label(egui::RichText::new(&a.reason).color(t.muted_foreground));
                    ui.add_space(8.0);
                    egui::Frame::new()
                        .fill(t.sidebar)
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&a.ddl)
                                        .monospace()
                                        .size(12.0)
                                        .color(t.foreground),
                                )
                                .wrap()
                                .selectable(true),
                            );
                        });
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(
                            "A guess from the statement text, without row counts: check it with EXPLAIN \
                             ANALYZE. On a busy PostgreSQL table CREATE INDEX CONCURRENTLY avoids blocking \
                             writes (it can't run inside a transaction). An index slows down writes.",
                        )
                        .size(11.0)
                        .color(t.subtle),
                    );
                }
                Err(e) => {
                    ui.label(egui::RichText::new(e).color(t.muted_foreground));
                }
            }
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(theme::outline_button("Close")).clicked() {
                    action = Some(AdviceAction::Close);
                }
                if let Ok(a) = advice {
                    if ui
                        .add(theme::primary_button(Some(icon::CODE), "Open in editor"))
                        .clicked()
                    {
                        action = Some(AdviceAction::Open(a.ddl.clone()));
                    }
                    if ui.add(theme::outline_button("Copy")).clicked() {
                        ui.ctx().copy_text(a.ddl.clone());
                    }
                }
            });
        });
    if action.is_none() && modal.should_close() {
        action = Some(AdviceAction::Close);
    }
    action
}

/// "Server": headline numbers as tiles, then the biggest tables and the least used indexes.
pub(super) fn server_panel(
    ui: &mut egui::Ui,
    tab_id: u64,
    info: Option<&Result<crate::db::ServerInfo, String>>,
    busy: bool,
) {
    let t = theme::tokens();
    let info = match info {
        None => {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                if busy {
                    ui.spinner();
                }
            });
            return;
        }
        Some(Err(e)) => return error_banner(ui, e),
        Some(Ok(i)) => i,
    };
    egui::ScrollArea::vertical()
        .id_salt(("server", tab_id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                        for (label, value) in &info.facts {
                            egui::Frame::new()
                                .fill(t.sidebar)
                                .stroke(egui::Stroke::new(1.0, t.border))
                                .inner_margin(egui::Margin::symmetric(12, 8))
                                .show(ui, |ui| {
                                    ui.set_min_width(140.0);
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(label).size(11.0).color(t.subtle));
                                        // The version string can be long (MySQL adds the build).
                                        let v: String = value.chars().take(28).collect();
                                        ui.label(egui::RichText::new(v).size(18.0).color(t.foreground))
                                            .on_hover_text(value);
                                    });
                                });
                        }
                    });
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("BIGGEST TABLES").size(11.0).color(t.subtle));
                    ui.add_space(4.0);
                    preview_rows(ui, tab_id ^ 1, &info.largest);
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("LEAST USED INDEXES")
                            .size(11.0)
                            .color(t.subtle),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Reads since the statistics started (in MySQL, since the table was \
                             opened; empty = no data). A rarely used index takes space and slows down writes, \
                             but the ones backing a foreign key are usually needed.",
                        )
                        .size(11.0)
                        .color(t.subtle),
                    );
                    ui.add_space(4.0);
                    if info.indexes.rows.is_empty() {
                        ui.label(
                            egui::RichText::new("No indexes besides primary and unique keys")
                                .color(t.muted_foreground),
                        );
                    } else {
                        preview_rows(ui, tab_id ^ 2, &info.indexes);
                    }
                });
        });
}

/// `Some(true)` to go ahead, `Some(false)` to give up, `None` while the dialog stays open.
pub(super) fn confirm_kill(ctx: &egui::Context, tab_id: u64, id: i64, terminate: bool) -> Option<bool> {
    let t = theme::tokens();
    let mut answer = None;
    let (title, text, button) = if terminate {
        (
            "Kill connection",
            "The session is disconnected; an open transaction in it is rolled back.",
            "Kill",
        )
    } else {
        (
            "Cancel query",
            "The statement running in that session is interrupted; the connection stays open.",
            "Cancel query",
        )
    };
    let modal = egui::Modal::new(egui::Id::new(("confirm_kill", tab_id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(400.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::WARNING).size(16.0).color(t.destructive));
                ui.label(
                    egui::RichText::new(format!("{title} — session {id}"))
                        .size(15.0)
                        .color(t.foreground),
                );
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new(text).color(t.muted_foreground));
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(danger_button(button)).clicked() {
                    answer = Some(true);
                }
                if ui.add(theme::outline_button("Back")).clicked() {
                    answer = Some(false);
                }
            });
        });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    answer
}
