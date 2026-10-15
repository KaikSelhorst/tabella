//! "Run .sql file": a file run statement by statement, with progress.

use super::*;
use std::fmt::Write as _;

/// A `.sql` file run statement by statement: progress while it goes, then where it stopped.
pub(super) fn script_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let t = theme::tokens();
    let TabKind::Script(st) = &mut tab.kind else {
        return;
    };
    let (mut start, mut cancel) = (false, false);
    egui::ScrollArea::vertical()
        .id_salt(("script", tab.id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .fill(t.background)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::icon(icon::PLAY).size(15.0).color(t.muted_foreground));
                        ui.label(egui::RichText::new("Run .sql file").size(15.0).color(t.foreground));
                    });
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(st.path.display().to_string())
                            .monospace()
                            .color(t.foreground),
                    );
                    let here = match s.cfg.kind {
                        crate::db::Kind::Postgres => format!("{} · {}", s.current_db, s.current_schema),
                        crate::db::Kind::MySql | crate::db::Kind::Sqlite => s.current_db.clone(),
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · on {} ({})",
                            super::palette::format_bytes(st.size as i64),
                            here,
                            s.cfg.name
                        ))
                        .size(12.0)
                        .color(t.muted_foreground),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().interact_size.y = 28.0;
                        if st.running {
                            if ui.add(theme::outline_button("Cancel")).clicked() {
                                cancel = true;
                            }
                        } else {
                            let label = if st.outcome.is_some() { "Run again" } else { "Run" };
                            if ui.add(theme::primary_button(Some(icon::PLAY), label)).clicked() {
                                start = true;
                            }
                        }
                        let hint = match s.cfg.kind {
                            crate::db::Kind::Postgres | crate::db::Kind::Sqlite => {
                                "All or nothing: on the first error (or on cancel) nothing from the file stays in the database"
                            }
                            crate::db::Kind::MySql => {
                                "In MySQL, each CREATE/ALTER/DROP commits what came before it: on an error, only what ran \
                                 after the last of them is rolled back"
                            }
                        };
                        ui.add_enabled(!st.running, egui::Checkbox::new(&mut st.transaction, "in a transaction"))
                            .on_hover_text(hint);
                    });
                    ui.label(
                        egui::RichText::new(
                            "Reads the file bit by bit and stops at the first error. Understands DELIMITER (MySQL), \
                             $$ blocks and pg_dump's COPY … FROM stdin; psql commands (\\connect…) are skipped.",
                        )
                        .size(11.0)
                        .color(t.subtle),
                    );
                    if !st.running && st.outcome.is_none() {
                        return;
                    }
                    ui.add_space(12.0);
                    let p = st.progress;
                    let frac = if st.size == 0 { 1.0 } else { (p.bytes as f32 / st.size as f32).min(1.0) };
                    ui.horizontal(|ui| {
                        let fill = match &st.outcome {
                            Some(Some(_)) => t.destructive,
                            _ => t.lime,
                        };
                        ui.add(egui::ProgressBar::new(frac).desired_width(320.0).desired_height(6.0).fill(fill));
                        let took = st.took.or_else(|| st.started.map(|at| at.elapsed())).unwrap_or_default();
                        let plural = |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
                        let mut text = format!(
                            "{:.0}% · {} · line {} · {:.1}s",
                            frac * 100.0,
                            plural(p.statements, "statement", "statements"),
                            p.line,
                            took.as_secs_f32()
                        );
                        if p.copied > 0 {
                            let _ = write!(text, " · {} via COPY", plural(p.copied, "row", "rows"));
                        }
                        if p.skipped > 0 {
                            let skipped = plural(p.skipped, "psql command skipped", "psql commands skipped");
                            let _ = write!(text, " · {skipped}");
                        }
                        ui.label(egui::RichText::new(text).size(12.0).color(t.muted_foreground));
                    });
                    if st.running {
                        // The worker reports a few times a second; keep the clock moving.
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                        return;
                    }
                    ui.add_space(8.0);
                    match &st.outcome {
                        Some(None) => {
                            ui.horizontal(|ui| {
                                ui.label(theme::icon(icon::CHECK).size(16.0).color(t.lime));
                                let done = if st.transaction { "File run and committed" } else { "File run" };
                                ui.label(egui::RichText::new(done).color(t.foreground));
                            });
                        }
                        Some(Some(e)) => {
                            let undone = if st.transaction {
                                " Nothing from the file stayed in the database (transaction rolled back)."
                            } else {
                                " What ran before that stays in the database."
                            };
                            let title = if e.cancelled {
                                format!("Cancelled at line {}.{undone}", e.line)
                            } else {
                                format!("Error at line {}.{undone}", e.line)
                            };
                            ui.horizontal(|ui| {
                                ui.label(theme::icon(icon::WARNING).size(16.0).color(t.destructive));
                                ui.label(egui::RichText::new(title).color(t.foreground));
                            });
                            if !e.cancelled {
                                ui.add_space(4.0);
                                error_banner(ui, &e.message);
                            }
                            if !e.statement.is_empty() {
                                ui.add_space(4.0);
                                egui::Frame::new()
                                    .fill(t.card)
                                    .stroke(egui::Stroke::new(1.0, t.border))
                                    .inner_margin(egui::Margin::same(8))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(
                                            egui::RichText::new(&e.statement)
                                                .monospace()
                                                .size(12.0)
                                                .color(t.foreground),
                                        );
                                    });
                            }
                        }
                        None => {}
                    }
                });
        });
    if start {
        app::start_script(tab, s);
    } else if cancel {
        app::cancel_script(tab, s);
    }
}
