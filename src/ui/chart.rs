//! Quick chart of an editor result: one numeric column against another, as bars or a line.
//! One series, one axis starting at zero, recessive grid, values on hover.

use crate::ui::theme::A11y;
use eframe::egui::{self, Align2, FontId, Pos2, Rect, RichText, Stroke, vec2};

use super::{grid::display_value, palette, theme};
use crate::db::QueryResult;
use crate::values::{self, ValueKind};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChartKind {
    Bars,
    Line,
}

/// Which columns to plot; kept per tab.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartSpec {
    pub x: usize,
    pub y: usize,
    pub kind: ChartKind,
}

/// Bars beyond this are left out (a note says how many).
const MAX_BARS: usize = 80;
const LEFT: f32 = 64.0;
const BOTTOM: f32 = 28.0;
const TOP: f32 = 12.0;
const RIGHT: f32 = 16.0;

/// A sensible first chart: labels from the first non-numeric column, values from the first
/// numeric one; a line when the labels are dates or numbers.
pub fn default_spec(res: &QueryResult, kinds: &[ValueKind]) -> Option<ChartSpec> {
    let y = (0..res.columns.len())
        .find(|&i| kinds[i] == ValueKind::Number && i != 0)
        .or_else(|| (0..res.columns.len()).find(|&i| kinds[i] == ValueKind::Number))?;
    let x = (0..res.columns.len())
        .find(|&i| i != y && kinds[i] != ValueKind::Number)
        .or_else(|| (0..res.columns.len()).find(|&i| i != y))
        .unwrap_or(y);
    let kind = if matches!(kinds[x], ValueKind::Date | ValueKind::Number) {
        ChartKind::Line
    } else {
        ChartKind::Bars
    };
    Some(ChartSpec { x, y, kind })
}

/// "Nice" axis ticks covering `lo..hi`: steps of 1, 2 or 5 times a power of ten.
pub fn ticks(lo: f64, hi: f64) -> Vec<f64> {
    if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
        return vec![lo];
    }
    let raw = (hi - lo) / 4.0;
    // Too narrow (a quarter of it rounds to zero) or too wide (past f64) for nice steps: its
    // ends. A step of zero would ask for every i64 in between.
    if !raw.is_normal() {
        return if lo.is_finite() && hi.is_finite() {
            vec![lo, hi]
        } else {
            vec![0.0]
        };
    }
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag);
    // From the tick at or below `lo` to the first one at or above `hi`, as multiples of the
    // step (rounded, so 3 × 0.2 reads 0.6, not 0.6000000000000001; not tiny steps, which the
    // rounding would make all zero, nor huge ones, where it would overflow).
    let (first, last) = ((lo / step).floor() as i64, (hi / step - 1e-9).ceil() as i64);
    let round = |v: f64| {
        if (1e-6..1e15).contains(&step) {
            (v * 1e9).round() / 1e9
        } else {
            v
        }
    };
    // The tick past the largest f64 is that one.
    (first..=last)
        .map(|k| round(k as f64 * step).clamp(-f64::MAX, f64::MAX))
        .collect()
}

/// Axis label: 1,2 mi / 15 mil / 3,5.
fn axis_label(v: f64) -> String {
    if v.abs() >= 10_000.0 {
        palette::compact_count(v.round() as i64)
    } else {
        values::format_number(v)
    }
}

/// Returns false when the user closes the chart.
pub fn chart(
    ui: &mut egui::Ui,
    tab_id: u64,
    res: &QueryResult,
    kinds: &[ValueKind],
    spec: &mut ChartSpec,
) -> bool {
    let t = theme::tokens();
    let mut open = true;
    // Controls: X, Y, form, close — one row above the plot.
    ui.horizontal(|ui| {
        let pick = |ui: &mut egui::Ui, id: &str, label: &str, value: &mut usize, numeric_only: bool| {
            ui.label(RichText::new(label).size(12.0).color(t.subtle));
            egui::ComboBox::from_id_salt((id, tab_id))
                .selected_text(RichText::new(&res.columns[*value]).monospace())
                .show_ui(ui, |ui| {
                    for (i, c) in res.columns.iter().enumerate() {
                        if numeric_only && kinds[i] != ValueKind::Number {
                            continue;
                        }
                        ui.selectable_value(value, i, RichText::new(c).monospace());
                    }
                });
        };
        pick(ui, "chart_x", "label", &mut spec.x, false);
        pick(ui, "chart_y", "value", &mut spec.y, true);
        ui.add_space(8.0);
        ui.spacing_mut().item_spacing.x = 0.0;
        for (k, label) in [(ChartKind::Bars, "Bars"), (ChartKind::Line, "Line")] {
            if ui
                .add(egui::Button::new(label).selected(spec.kind == k))
                .clicked()
            {
                spec.kind = k;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new(theme::icon(super::icon::X).color(t.muted_foreground)).frame(false))
                .tip("Close chart")
                .clicked()
            {
                open = false;
            }
        });
    });

    // Points in result order; rows whose value isn't a number are skipped, and so are
    // Infinity and NaN (Postgres floats have them): there's no place for them on the axis.
    let mut points: Vec<(String, f64)> = res
        .rows
        .iter()
        .filter_map(|r| {
            let y = r
                .cell(spec.y)?
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|y| y.is_finite())?;
            let x = r
                .cell(spec.x)
                .map_or("NULL".to_string(), |v| display_value(kinds[spec.x], v));
            Some((x, y))
        })
        .collect();
    let left_out = match spec.kind {
        ChartKind::Bars if points.len() > MAX_BARS => {
            let n = points.len() - MAX_BARS;
            points.truncate(MAX_BARS);
            n
        }
        _ => 0,
    };

    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), ui.available_height().max(120.0)),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    if points.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "No numeric value to show",
            FontId::proportional(13.0),
            t.subtle,
        );
        return open;
    }
    let plot = Rect::from_min_max(rect.min + vec2(LEFT, TOP), rect.max - vec2(RIGHT, BOTTOM));
    // Bars and lines both measure from zero: the axis always includes it.
    let lo = points.iter().map(|p| p.1).fold(0.0f64, f64::min);
    let hi = points.iter().map(|p| p.1).fold(0.0f64, f64::max);
    let ticks = ticks(lo, if hi == lo { lo + 1.0 } else { hi });
    let (t0, t1) = (ticks[0], *ticks.last().unwrap());
    // In halves: from -f64::MAX to f64::MAX the full differences overflow (∞ / ∞ is NaN).
    let y_at = |v: f64| {
        let share = (v * 0.5 - t0 * 0.5) / (t1 * 0.5 - t0 * 0.5).max(f64::EPSILON);
        plot.bottom() - share as f32 * plot.height()
    };
    let label_font = FontId::proportional(12.0);

    // Grid: faint lines at the ticks, the zero line a step stronger.
    for v in &ticks {
        let y = y_at(*v);
        let stroke = if *v == 0.0 {
            Stroke::new(1.0, t.muted_foreground.gamma_multiply(0.5))
        } else {
            Stroke::new(1.0, t.border)
        };
        painter.hline(plot.x_range(), y, stroke);
        painter.text(
            Pos2::new(plot.left() - 8.0, y),
            Align2::RIGHT_CENTER,
            axis_label(*v),
            label_font.clone(),
            t.subtle,
        );
    }

    let n = points.len();
    let slot = plot.width() / n as f32;
    let x_center = |i: usize| match spec.kind {
        ChartKind::Bars => plot.left() + slot * (i as f32 + 0.5),
        ChartKind::Line if n == 1 => plot.center().x,
        ChartKind::Line => plot.left() + plot.width() * i as f32 / (n - 1) as f32,
    };
    let hover = ui
        .ctx()
        .pointer_hover_pos()
        .filter(|p| response.contains_pointer() && plot.expand2(vec2(0.0, 8.0)).contains(*p));
    let hovered = hover.map(|p| {
        let i = match spec.kind {
            ChartKind::Bars => ((p.x - plot.left()) / slot).floor() as isize,
            ChartKind::Line if n == 1 => 0,
            ChartKind::Line => ((p.x - plot.left()) / plot.width() * (n - 1) as f32).round() as isize,
        };
        i.clamp(0, n as isize - 1) as usize
    });

    let color = t.accent_text;
    match spec.kind {
        ChartKind::Bars => {
            // Thin bars with a 2px gap; square ends.
            let w = (slot * 0.7).clamp(1.0, 48.0);
            for (i, (_, v)) in points.iter().enumerate() {
                let cx = x_center(i);
                let (a, b) = (y_at(0.0f64.max(t0)), y_at(*v));
                let bar = Rect::from_x_y_ranges(
                    cx - w / 2.0..=cx + w / 2.0 - 2.0f32.min(w / 4.0),
                    a.min(b)..=a.max(b),
                );
                let fill = if hovered == Some(i) {
                    color
                } else {
                    color.gamma_multiply(0.75)
                };
                painter.rect_filled(bar, 0.0, fill);
            }
        }
        ChartKind::Line => {
            let line: Vec<Pos2> = points
                .iter()
                .enumerate()
                .map(|(i, (_, v))| Pos2::new(x_center(i), y_at(*v)))
                .collect();
            painter.add(egui::Shape::line(line, Stroke::new(2.0, color)));
        }
    }

    // X labels: under each bar when they fit, otherwise a handful spread evenly.
    let room = match spec.kind {
        ChartKind::Bars => slot,
        ChartKind::Line => plot.width() / n.max(2) as f32,
    };
    let every = ((90.0 / room).ceil() as usize).max(1);
    for (i, (x, _)) in points.iter().enumerate() {
        let last = i + 1 == n;
        let line_end = last && spec.kind == ChartKind::Line;
        if i % every != 0 && !line_end {
            continue;
        }
        // The line's last label is always shown; regular ones too close to it give way.
        if spec.kind == ChartKind::Line && !last && x_center(n - 1) - x_center(i) < 90.0 {
            continue;
        }
        let text: String = if x.chars().count() > 14 {
            format!("{}…", x.chars().take(13).collect::<String>())
        } else {
            x.clone()
        };
        let align = match (spec.kind, i, last) {
            (ChartKind::Line, 0, _) => Align2::LEFT_TOP,
            (ChartKind::Line, _, true) => Align2::RIGHT_TOP,
            _ => Align2::CENTER_TOP,
        };
        painter.text(
            Pos2::new(x_center(i), plot.bottom() + 6.0),
            align,
            text,
            label_font.clone(),
            t.subtle,
        );
    }

    // Hover: crosshair + marker (line) or the bar itself, and the exact value.
    if let Some(i) = hovered {
        let (x, v) = &points[i];
        let p = Pos2::new(x_center(i), y_at(*v));
        if spec.kind == ChartKind::Line {
            painter.vline(
                p.x,
                plot.y_range(),
                Stroke::new(1.0, t.muted_foreground.gamma_multiply(0.6)),
            );
            painter.circle(p, 4.0, color, Stroke::new(2.0, t.background));
        }
        let tip = format!("{x}\n{}: {}", res.columns[spec.y], values::format_number(*v));
        let galley = painter.layout_no_wrap(tip, FontId::proportional(12.0), t.foreground);
        let size = galley.size() + vec2(16.0, 10.0);
        let mut at = p + vec2(12.0, -size.y - 8.0);
        at.x = at.x.min(rect.right() - size.x);
        at.y = at.y.max(rect.top());
        let bg = Rect::from_min_size(at, size);
        painter.rect(
            bg,
            0.0,
            t.card,
            Stroke::new(1.0, t.border),
            egui::StrokeKind::Inside,
        );
        painter.galley(at + vec2(8.0, 5.0), galley, t.foreground);
    }
    if left_out > 0 {
        painter.text(
            rect.right_top() + vec2(-RIGHT, 0.0),
            Align2::RIGHT_TOP,
            format!("first {MAX_BARS} bars · {left_out} left out of the chart"),
            label_font,
            t.subtle,
        );
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_ticks_cover_the_range_from_zero() {
        assert_eq!(ticks(0.0, 97.0), [0.0, 50.0, 100.0]);
        assert_eq!(ticks(0.0, 1234.0), [0.0, 500.0, 1000.0, 1500.0]);
        assert_eq!(ticks(-30.0, 80.0), [-50.0, 0.0, 50.0, 100.0]);
        assert_eq!(ticks(0.0, 0.8), [0.0, 0.2, 0.4, 0.6, 0.8]);
        assert_eq!(ticks(0.0, 100.0), [0.0, 50.0, 100.0]);
    }

    #[test]
    fn picks_labels_and_values() {
        let res = QueryResult {
            columns: vec!["id".into(), "city".into(), "total".into()],
            ..Default::default()
        };
        let kinds = [ValueKind::Number, ValueKind::Text, ValueKind::Number];
        assert_eq!(
            default_spec(&res, &kinds),
            Some(ChartSpec {
                x: 1,
                y: 2,
                kind: ChartKind::Bars
            })
        );
        let kinds = [ValueKind::Date, ValueKind::Number, ValueKind::Text];
        assert_eq!(
            default_spec(&res, &kinds).map(|s| (s.x, s.y, s.kind)),
            Some((0, 1, ChartKind::Line))
        );
        assert_eq!(default_spec(&res, &[ValueKind::Text; 3]), None);
    }

    /// Any range the data can make (it always takes in zero): a few finite ticks covering it.
    #[test]
    fn ticks_for_any_range() {
        let ends = [
            0.0,
            1e-300,
            5e-324,
            0.1,
            1.0,
            7.0,
            1e6,
            1e15,
            9.2e18,
            1e300,
            f64::MAX,
        ];
        for lo in ends.map(|v: f64| -v) {
            for hi in ends {
                let t = ticks(lo, hi);
                assert!(!t.is_empty() && t.len() <= 12, "{lo}..{hi}: {t:?}");
                assert!(t.iter().all(|v| v.is_finite()), "{lo}..{hi}: {t:?}");
                // Covered, but for a millionth of the range (floats).
                let slack = (hi - lo) * 1e-6;
                if hi > lo && slack.is_finite() {
                    assert!(
                        t[0] <= lo + slack && *t.last().unwrap() >= hi - slack,
                        "{lo}..{hi}: {t:?}"
                    );
                }
            }
        }
    }

    /// Values a server really returns (`'Infinity'::float`, NaN, the extremes) chart without
    /// panicking, as bars and as a line.
    #[test]
    fn charts_extreme_values() {
        let mut res = QueryResult {
            columns: vec!["k".into(), "v".into()],
            ..Default::default()
        };
        for (i, v) in [
            "Infinity",
            "-Infinity",
            "NaN",
            "1e308",
            "-1e308",
            "0",
            "5e-324",
            "x",
        ]
        .iter()
        .enumerate()
        {
            res.rows.push([Some(i.to_string()), Some(v.to_string())]);
        }
        let kinds = [ValueKind::Text, ValueKind::Number];
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        for kind in [ChartKind::Bars, ChartKind::Line] {
            for rows in [1, 2, res.rows.len()] {
                let mut part = res.clone();
                part.rows.truncate(rows);
                let mut spec = ChartSpec { x: 0, y: 1, kind };
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 400.0),
                    )),
                    ..Default::default()
                };
                let mut out = ctx.run_ui(input, |ui| {
                    chart(ui, 1, &part, &kinds, &mut spec);
                });
                out.textures_delta.clear();
                let mesh = ctx.tessellate(out.shapes, out.pixels_per_point);
                for m in &mesh {
                    if let egui::epaint::Primitive::Mesh(m) = &m.primitive {
                        assert!(
                            m.vertices
                                .iter()
                                .all(|v| v.pos.x.is_finite() && v.pos.y.is_finite()),
                            "{kind:?} {rows}"
                        );
                    }
                }
            }
        }
    }
}
