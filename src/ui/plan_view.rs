//! EXPLAIN as a tree: one row per step, with what it costs on its own and a bar showing its share,
//! so the expensive step stands out without reading numbers.

use eframe::egui::{self, Align, Layout, RichText};
use std::fmt::Write as _;

use super::{icon, palette, theme};
use crate::plan::{Node, Plan};

/// Width of each number column on the right.
const NUM_WIDTH: f32 = 96.0;
/// Indentation per tree level.
const INDENT: f32 = 16.0;
/// Full scans above this many rows get a warning.
const BIG_SCAN: f64 = 10_000.0;

/// Returns true when the user closes the plan.
pub enum PlanAction {
    Close,
    OpenSql(String),
}

/// `hints`: suggested `CREATE INDEX` statements with what they're for.
pub fn plan_view(ui: &mut egui::Ui, plan: &Plan, hints: &[(String, String)]) -> Option<PlanAction> {
    let t = theme::tokens();
    let mut close = false;
    let mut open_sql = None;
    // What each bar measures: real time when the statement ran, planner cost otherwise.
    let weight = |n: &Node| if plan.analyzed { n.self_ms() } else { n.self_cost() };
    let mut max = 0.0f64;
    plan.root
        .walk(0, &mut |n, _| max = max.max(weight(n).unwrap_or(0.0)));

    egui::Frame::new()
        .fill(t.sidebar)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::TREE_VIEW).color(t.muted_foreground));
                ui.label(RichText::new("Execution plan").color(t.foreground));
                let kind = if plan.analyzed {
                    "actual (ANALYZE)"
                } else {
                    "estimated"
                };
                ui.label(RichText::new(kind).size(12.0).color(t.subtle));
                if let Some(ms) = plan.planning_ms {
                    ui.label(
                        RichText::new(format!("planning {}", ms_text(ms)))
                            .size(12.0)
                            .color(t.subtle),
                    );
                }
                if let Some(ms) = plan.execution_ms {
                    ui.label(
                        RichText::new(format!("execution {}", ms_text(ms)))
                            .size(12.0)
                            .color(t.subtle),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let x = theme::icon_text(icon::X, "Close plan", t.muted_foreground, t.foreground);
                    if ui.add(egui::Button::new(x).frame(false)).clicked() {
                        close = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(theme::icon(icon::COPY).color(t.muted_foreground)).frame(false),
                        )
                        .on_hover_text("Copy plan as text")
                        .clicked()
                    {
                        ui.ctx().copy_text(plan_text(plan));
                    }
                });
            });
        });

    egui::ScrollArea::both()
        .id_salt("plan")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    let numbers = if plan.analyzed {
                        ["self time", "est. rows", "actual rows"]
                    } else {
                        ["self cost", "est. rows", ""]
                    };
                    row_layout(
                        ui,
                        |ui| {
                            ui.label(RichText::new("ETAPA").size(11.0).color(t.subtle));
                        },
                        |ui| {
                            for h in numbers.iter().rev() {
                                number_cell(ui, RichText::new(*h).size(11.0).color(t.subtle));
                            }
                        },
                    );
                    ui.add_space(4.0);
                    plan.root
                        .walk(0, &mut |n, depth| node_row(ui, plan, n, depth, weight(n), max));
                    if !hints.is_empty() {
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            ui.label(theme::icon(icon::LIGHTBULB).color(t.warning));
                            ui.label(RichText::new("INDEX SUGGESTION").size(11.0).color(t.subtle));
                        });
                        ui.label(
                            RichText::new(
                                "Large tables read in full to apply a filter. An index on the compared columns \
                                 usually avoids that; double-check before creating it in production.",
                            )
                            .size(11.0)
                            .color(t.subtle),
                        );
                        ui.add_space(4.0);
                        for (what, sql) in hints {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(what).color(t.muted_foreground));
                                ui.add(egui::Label::new(RichText::new(sql).monospace().size(12.0).color(t.foreground)).truncate());
                                if ui
                                    .button(theme::icon_text(icon::CODE, "Open in editor", t.muted_foreground, t.foreground))
                                    .clicked()
                                {
                                    open_sql = Some(sql.clone());
                                }
                            });
                        }
                    }
                });
        });
    if close {
        Some(PlanAction::Close)
    } else {
        open_sql.map(PlanAction::OpenSql)
    }
}

/// A row with free content on the left and fixed-width numbers on the right.
fn row_layout(
    ui: &mut egui::Ui,
    left: impl FnOnce(&mut egui::Ui),
    right: impl FnOnce(&mut egui::Ui),
) -> egui::Rect {
    ui.horizontal(|ui| {
        let width = ui.available_width().max(640.0);
        ui.set_min_width(width);
        ui.allocate_ui_with_layout(
            egui::vec2(width - 3.0 * NUM_WIDTH, 20.0),
            Layout::left_to_right(Align::Center),
            left,
        );
        ui.with_layout(Layout::right_to_left(Align::Center), right);
    })
    .response
    .rect
}

fn number_cell(ui: &mut egui::Ui, text: RichText) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(NUM_WIDTH, 20.0),
        Layout::right_to_left(Align::Center),
        |ui| {
            ui.set_min_width(NUM_WIDTH);
            ui.label(text)
        },
    )
    .inner
}

fn node_row(ui: &mut egui::Ui, plan: &Plan, n: &Node, depth: usize, weight: Option<f64>, max: f64) {
    let t = theme::tokens();
    let share = match weight {
        Some(w) if max > 0.0 => (w / max) as f32,
        _ => 0.0,
    };
    let bar = ui.painter().add(egui::Shape::Noop);
    let rows = n.actual_rows.or(n.est_rows).unwrap_or(0.0);
    let big_scan = n.is_full_scan() && rows >= BIG_SCAN;
    // Estimates off by 10x or more usually explain a bad plan (stale statistics).
    let misestimate = match (n.est_rows, n.actual_rows) {
        (Some(e), Some(a)) => (a.max(1.0) / e.max(1.0)).max(e.max(1.0) / a.max(1.0)) >= 10.0,
        _ => false,
    };
    let rect = row_layout(
        ui,
        |ui| {
            ui.add_space(depth as f32 * INDENT);
            if depth > 0 {
                // Elbow linking the step to its parent.
                let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 20.0), egui::Sense::hover());
                let stroke = egui::Stroke::new(1.0, t.border);
                let x = r.left() + 2.0;
                ui.painter().line_segment(
                    [egui::pos2(x, r.top() - 4.0), egui::pos2(x, r.center().y)],
                    stroke,
                );
                ui.painter().line_segment(
                    [egui::pos2(x, r.center().y), egui::pos2(r.right(), r.center().y)],
                    stroke,
                );
            }
            let color = if big_scan { t.warning } else { t.foreground };
            ui.add(egui::Label::new(RichText::new(&n.label).monospace().color(color)).truncate());
            if big_scan {
                ui.label(theme::icon(icon::WARNING).size(12.0).color(t.warning))
                    .on_hover_text("Reads the whole table: an index on the filter columns may avoid that");
            }
        },
        |ui| {
            if plan.analyzed {
                let real = n.actual_rows.map(fmt_rows).unwrap_or_default();
                let color = if misestimate {
                    t.warning
                } else {
                    t.muted_foreground
                };
                let r = number_cell(ui, RichText::new(real).monospace().size(12.0).color(color));
                if misestimate {
                    r.on_hover_text("Estimate far from the actual count: run ANALYZE on the table to refresh its statistics");
                }
            } else {
                number_cell(ui, RichText::new(""));
            }
            number_cell(
                ui,
                RichText::new(n.est_rows.map(fmt_rows).unwrap_or_default())
                    .monospace()
                    .size(12.0)
                    .color(t.muted_foreground),
            );
            let main = if plan.analyzed {
                n.self_ms().map(ms_text)
            } else {
                n.self_cost().map(|c| format!("{c:.1}"))
            };
            number_cell(
                ui,
                RichText::new(main.unwrap_or_default())
                    .monospace()
                    .size(12.0)
                    .color(t.foreground),
            );
        },
    );
    // Share of the total as a bar behind the row: the hottest steps glow.
    if share > 0.0 {
        let mut r = rect;
        r.set_width(rect.width() * share);
        let color = if share > 0.5 {
            theme::mix(t.destructive, 0.22, t.background)
        } else {
            theme::mix(t.lime, 0.10, t.background)
        };
        ui.painter().set(bar, egui::Shape::rect_filled(r, 0.0, color));
    }
    if !n.details.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * INDENT + 20.0);
            ui.label(
                RichText::new(n.details.join(" · "))
                    .monospace()
                    .size(11.0)
                    .color(t.subtle),
            );
        });
    }
    ui.add_space(2.0);
}

fn fmt_rows(r: f64) -> String {
    palette::compact_count(r.round() as i64)
}

fn ms_text(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else if ms >= 10.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{ms:.2} ms")
    }
}

/// The plan as indented text, for pasting into a chat or an issue.
fn plan_text(plan: &Plan) -> String {
    let mut out = String::new();
    plan.root.walk(0, &mut |n, depth| {
        let mut line = format!("{}-> {}", "   ".repeat(depth), n.label);
        if let Some(c) = n.cost {
            let _ = write!(line, "  (cost={c:.1}");
            if let Some(r) = n.est_rows {
                let _ = write!(line, " rows={r}");
            }
            line.push(')');
        }
        if let (Some(ms), Some(r)) = (n.actual_ms, n.actual_rows) {
            let _ = write!(line, " (actual {} rows={r})", ms_text(ms));
        }
        out.push_str(&line);
        out.push('\n');
        for d in &n.details {
            let _ = writeln!(out, "{}     {d}", "   ".repeat(depth));
        }
    });
    out
}
