//! ER diagram tab: boxes and foreign-key lines on a canvas that pans (drag), zooms (wheel) and
//! lets boxes be moved by their title. Double-click a box to open its table.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, vec2};

use super::{icon, theme};
use crate::app::Session;
use crate::diagram::{BOX_W, DiagramState, MAX_COLS, ROW_H};

pub enum DiagramAction {
    OpenTable(String),
    /// Show another schema.
    Schema(String),
}

/// Below this zoom only the table names are drawn.
const DETAIL_ZOOM: f32 = 0.45;

pub fn diagram_tab(
    ui: &mut egui::Ui,
    tab_id: u64,
    d: &mut DiagramState,
    busy: bool,
    s: &Session,
) -> Option<DiagramAction> {
    let t = theme::tokens();
    let mut action = diagram_header(ui, tab_id, d, busy, s);
    if !has_diagram(ui, d) {
        return action;
    }
    let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, t.background);
    if std::mem::take(&mut d.fit) {
        fit(d, rect);
    }
    let v = View {
        rect,
        pan: vec2(d.pan.0, d.pan.1),
        zoom: d.zoom,
    };
    // What the pointer is over: the topmost box.
    let pointer = ui.ctx().pointer_hover_pos().filter(|p| rect.contains(*p));
    let hovered: Option<usize> = pointer.and_then(|p| {
        d.tables
            .iter()
            .enumerate()
            .rev()
            .find(|(_, tb)| {
                v.screen(&d.positions, &tb.name, tb.height())
                    .is_some_and(|r| r.contains(p))
            })
            .map(|(i, _)| i)
    });
    move_and_zoom(ui, tab_id, d, &v, &resp, pointer);
    if (resp.double_clicked() || resp.triple_clicked())
        && let Some(i) = hovered
    {
        action = Some(DiagramAction::OpenTable(d.tables[i].name.clone()));
    }
    if hovered.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    // Lines first, boxes on top.
    paint_links(&painter, d, &v, hovered.map(|i| d.tables[i].name.as_str()));
    paint_boxes(&painter, d, &v, hovered);
    action
}

/// Where the canvas is on screen and how the diagram sits in it, as of this frame.
struct View {
    rect: Rect,
    pan: egui::Vec2,
    zoom: f32,
}

impl View {
    /// A box on screen, from the positions as they are when called.
    fn screen(
        &self,
        positions: &std::collections::HashMap<String, (f32, f32)>,
        name: &str,
        h: f32,
    ) -> Option<Rect> {
        let &(x, y) = positions.get(name)?;
        Some(Rect::from_min_size(
            self.rect.min + self.pan + vec2(x, y) * self.zoom,
            vec2(BOX_W, h) * self.zoom,
        ))
    }
}

/// Title, schema picker, "show unrelated tables", Fit, Rearrange and the counts.
fn diagram_header(
    ui: &mut egui::Ui,
    tab_id: u64,
    d: &mut DiagramState,
    busy: bool,
    s: &Session,
) -> Option<DiagramAction> {
    let t = theme::tokens();
    let mut action = None;
    let edge_count = d.schema_edges().len();
    egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::GRAPH).size(15.0).color(t.muted_foreground));
                ui.label(RichText::new("ER diagram").size(15.0).color(t.foreground));
                egui::ComboBox::from_id_salt(("diagram_schema", tab_id))
                    .selected_text(RichText::new(&d.schema).monospace())
                    .show_ui(ui, |ui| {
                        // Tables come sorted by schema, so `dedup` leaves each once.
                        let mut schemas: Vec<&str> = s.tables.iter().map(|t| t.schema.as_str()).collect();
                        schemas.dedup();
                        for sc in schemas {
                            if ui.selectable_label(sc == d.schema, RichText::new(sc).monospace()).clicked()
                                && sc != d.schema
                            {
                                action = Some(DiagramAction::Schema(sc.to_string()));
                            }
                        }
                    });
                if ui.checkbox(&mut d.show_all, "show unrelated tables").changed() {
                    d.build(&s.tables, &s.schema_cols);
                    d.fit = true;
                }
                if busy {
                    ui.spinner();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(theme::icon_text(icon::ARROWS_OUT, "Fit", t.muted_foreground, t.foreground))
                        .on_hover_text("Fit all tables on screen")
                        .clicked()
                    {
                        d.fit = true;
                    }
                    if ui
                        .button(theme::icon_text(icon::SHUFFLE, "Rearrange", t.muted_foreground, t.foreground))
                        .on_hover_text("Undo dragging and lay out again")
                        .clicked()
                    {
                        d.positions.clear();
                        d.build(&s.tables, &s.schema_cols);
                        d.fit = true;
                    }
                });
            });
            ui.label(
                RichText::new(format!(
                    "{} tables · {edge_count} links · {:.0}%  ·  wheel: zoom · drag the background or the title · \
                     double-click opens the table",
                    d.tables.len(),
                    d.zoom * 100.0
                ))
                .size(11.0)
                .color(t.subtle),
            );
        });
    action
}

/// Whether there is a diagram to draw; otherwise shows why not (loading, an error, no links).
fn has_diagram(ui: &mut egui::Ui, d: &DiagramState) -> bool {
    let t = theme::tokens();
    match &d.edges {
        None => {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| ui.spinner());
            false
        }
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(t.destructive));
            false
        }
        Some(Ok(_)) if d.tables.is_empty() => {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("No foreign keys between the tables of this schema.")
                        .color(t.muted_foreground),
                );
                ui.label(
                    RichText::new("Check \"show unrelated tables\" to see them all.")
                        .size(12.0)
                        .color(t.subtle),
                );
            });
            false
        }
        Some(Ok(_)) => true,
    }
}

/// Zoom and pan so that every box fits the canvas.
fn fit(d: &mut DiagramState, rect: Rect) {
    let Some((x0, y0, x1, y1)) = d.bounds() else {
        return;
    };
    let margin = 48.0;
    let zoom = ((rect.width() - margin) / (x1 - x0)).min((rect.height() - margin) / (y1 - y0));
    d.zoom = zoom.clamp(0.2, 1.0);
    let center = vec2(f32::midpoint(x0, x1), f32::midpoint(y0, y1)) * d.zoom;
    let pan = rect.size() / 2.0 - center;
    d.pan = (pan.x, pan.y);
}

/// Dragging a title moves that box; dragging anywhere else moves the view; the wheel zooms
/// around the pointer.
fn move_and_zoom(
    ui: &egui::Ui,
    tab_id: u64,
    d: &mut DiagramState,
    v: &View,
    resp: &egui::Response,
    pointer: Option<Pos2>,
) {
    let (rect, pan, zoom) = (v.rect, v.pan, v.zoom);
    let drag_id = egui::Id::new(("diagram_drag", tab_id));
    if resp.drag_started()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let on_title = d.tables.iter().rev().find(|tb| {
            v.screen(&d.positions, &tb.name, tb.height())
                .is_some_and(|r| Rect::from_min_size(r.min, vec2(r.width(), ROW_H * zoom)).contains(p))
        });
        let name = on_title.map(|tb| tb.name.clone());
        ui.data_mut(|m| m.insert_temp(drag_id, name));
    }
    if resp.dragged() {
        let delta = resp.drag_delta();
        match ui.data(|m| m.get_temp::<Option<String>>(drag_id)).flatten() {
            Some(name) => {
                if let Some(p) = d.positions.get_mut(&name) {
                    p.0 += delta.x / zoom;
                    p.1 += delta.y / zoom;
                }
            }
            None => d.pan = (d.pan.0 + delta.x, d.pan.1 + delta.y),
        }
    }
    if let Some(p) = pointer {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let new_zoom = (d.zoom * (1.0 + scroll * 0.0015)).clamp(0.2, 2.0);
            let world = (p - rect.min - pan) / d.zoom;
            let new_pan = p - rect.min - world * new_zoom;
            d.zoom = new_zoom;
            d.pan = (new_pan.x, new_pan.y);
        }
    }
}

/// The foreign-key lines, lit for the box under the pointer.
fn paint_links(painter: &egui::Painter, d: &DiagramState, v: &View, hovered_name: Option<&str>) {
    let t = theme::tokens();
    let table_of = |name: &str| d.tables.iter().find(|tb| tb.name == name);
    for e in d.schema_edges() {
        let (Some(src), Some(dst)) = (table_of(&e.table), table_of(&e.ref_table)) else {
            continue;
        };
        let (Some(a), Some(b)) = (
            v.screen(&d.positions, &src.name, src.height()),
            v.screen(&d.positions, &dst.name, dst.height()),
        ) else {
            continue;
        };
        let ya = a.top() + src.column_y(&e.column) * v.zoom;
        let yb = b.top() + dst.column_y(&e.ref_column) * v.zoom;
        let reach = 60.0 * v.zoom;
        let (p0, p3, c0, c1) = if src.name == dst.name {
            // Self-reference: a loop on the right side.
            let (p0, p3) = (Pos2::new(a.right(), ya), Pos2::new(a.right(), yb));
            (p0, p3, p0 + vec2(reach, 0.0), p3 + vec2(reach, 0.0))
        } else if b.center().x >= a.center().x {
            let (p0, p3) = (Pos2::new(a.right(), ya), Pos2::new(b.left(), yb));
            (p0, p3, p0 + vec2(reach, 0.0), p3 - vec2(reach, 0.0))
        } else {
            let (p0, p3) = (Pos2::new(a.left(), ya), Pos2::new(b.right(), yb));
            (p0, p3, p0 - vec2(reach, 0.0), p3 + vec2(reach, 0.0))
        };
        let lit = hovered_name.is_some_and(|h| h == src.name || h == dst.name);
        let stroke = if lit {
            Stroke::new(2.0, t.accent_text)
        } else {
            Stroke::new(1.5, t.muted_foreground.gamma_multiply(0.55))
        };
        painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
            [p0, c0, c1, p3],
            false,
            Color32::TRANSPARENT,
            stroke,
        ));
        // The referenced end gets a dot: "points at this".
        painter.circle_filled(p3, (3.0 * v.zoom).max(2.0), stroke.color);
    }
}

/// The boxes: title, and at a readable zoom the columns (foreign keys in their color) and types.
fn paint_boxes(painter: &egui::Painter, d: &DiagramState, v: &View, hovered: Option<usize>) {
    let t = theme::tokens();
    for (i, tb) in d.tables.iter().enumerate() {
        let Some(r) = v.screen(&d.positions, &tb.name, tb.height()) else {
            continue;
        };
        if !r.intersects(v.rect) {
            continue;
        }
        let lit = hovered == Some(i);
        painter.rect_filled(r, 0.0, t.card);
        let title = Rect::from_min_size(r.min, vec2(r.width(), ROW_H * v.zoom));
        painter.rect_filled(
            title,
            0.0,
            if lit {
                theme::mix(t.lime, 0.18, t.sidebar)
            } else {
                t.sidebar
            },
        );
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(1.0, if lit { t.accent_text } else { t.border }),
            egui::StrokeKind::Inside,
        );
        let clip = painter.with_clip_rect(r.intersect(v.rect));
        clip.text(
            title.left_center() + vec2(8.0 * v.zoom, 0.0),
            Align2::LEFT_CENTER,
            &tb.name,
            FontId::proportional(13.0 * v.zoom),
            t.foreground,
        );
        if v.zoom < DETAIL_ZOOM {
            continue;
        }
        let fk_cols: Vec<&str> = d
            .schema_edges()
            .into_iter()
            .filter(|e| e.table == tb.name)
            .map(|e| e.column.as_str())
            .collect();
        let font = FontId::monospace(11.5 * v.zoom);
        for (row, (col, ty)) in tb.columns.iter().take(MAX_COLS).enumerate() {
            let y = r.top() + ROW_H * v.zoom * (row as f32 + 1.5);
            let color = if fk_cols.contains(&col.as_str()) {
                t.info
            } else {
                t.foreground
            };
            clip.text(
                Pos2::new(r.left() + 8.0 * v.zoom, y),
                Align2::LEFT_CENTER,
                col,
                font.clone(),
                color,
            );
            let short: String = ty.chars().take(16).collect();
            clip.text(
                Pos2::new(r.right() - 8.0 * v.zoom, y),
                Align2::RIGHT_CENTER,
                short,
                FontId::proportional(10.5 * v.zoom),
                t.subtle,
            );
        }
        if tb.columns.len() > MAX_COLS {
            let y = r.top() + ROW_H * v.zoom * (MAX_COLS as f32 + 1.5);
            clip.text(
                Pos2::new(r.left() + 8.0 * v.zoom, y),
                Align2::LEFT_CENTER,
                format!("… {} more", tb.columns.len() - MAX_COLS),
                FontId::proportional(11.0 * v.zoom),
                t.subtle,
            );
        }
    }
}
