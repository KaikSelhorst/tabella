use eframe::egui::{self, Align, FontId, Key, Layout, RichText, Sense, TextEdit};
use std::collections::HashSet;

use egui_extras::{Column, TableBuilder};

use super::{icon, theme};
use crate::app::CopyAs;
use crate::app::{Edit, GridEvent};
use crate::db::{ColumnInfo, FkRef, IncomingFk, QueryResult};
use crate::sql::FilterOp;
use crate::values::{CellRange, SAMPLE_ROWS, ValueKind, column_info};

const HEADER_HEIGHT: f32 = 28.0;
fn row_height() -> f32 {
    24.0
}

fn cell_font_size() -> f32 {
    12.0
}
/// Rough advance of one character of the cell font, for sizing columns to their content.
/// Width of one monospace char at the grid font.
fn char_width() -> f32 {
    7.3
}
/// Space between a right-aligned number and its column's edge.
const NUMBER_PAD: f32 = 4.0;
const MIN_COL_WIDTH: f32 = 112.0;
const MAX_COL_WIDTH: f32 = 420.0;

/// Display form of a value: booleans as words, newlines as a visible glyph.
pub fn display_value(kind: ValueKind, v: &str) -> String {
    match (kind, v) {
        (ValueKind::Bool, "t" | "1" | "true") => "true".to_string(),
        (ValueKind::Bool, "f" | "0" | "false") => "false".to_string(),
        _ => v.replace('\n', icon::ARROW_ELBOW_DOWN_LEFT),
    }
}

/// Initial column width from the header and a sample of values, so short columns stay narrow.
fn column_width(name: &str, data_type: Option<&str>, samples: impl Iterator<Item = Option<usize>>) -> f32 {
    // Room for the whole header: name, type, and the key/sort icons.
    let header = (name.chars().count() + data_type.map_or(0, |t| t.chars().count() + 2) + 4) as f32;
    // Values get a couple of characters of breathing room.
    let longest = samples
        .map(|n| n.unwrap_or(4) as f32 + 2.0)
        .fold(header, f32::max);
    (longest * char_width() + 24.0).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH)
}

/// Painter for a cell that may draw into the gap between columns.
///
/// Columns clip to their own rect, so fills stopped short of the column separator egui_extras
/// draws in the gap, leaving a dark seam. This keeps the vertical clip (rows still scroll under
/// the header) but widens the horizontal one to the whole table.
fn gap_painter(ui: &egui::Ui, table_rect: egui::Rect) -> egui::Painter {
    let clip = egui::Rect::from_x_y_ranges(table_rect.x_range(), ui.clip_rect().y_range());
    ui.painter()
        .with_clip_rect(clip.intersect(ui.ctx().content_rect()))
}

/// The cell's area including half the column gap on each side, so neighbors meet at the separator.
fn full_cell(ui: &egui::Ui) -> egui::Rect {
    ui.max_rect()
        .expand2(egui::vec2(ui.spacing().item_spacing.x * 0.5, 0.0))
}

/// Cell hairlines. Backgrounds are painted per row (see `row_background`), not per cell:
/// per-cell fills left a seam in the gap between columns.
fn cell_chrome(ui: &egui::Ui, table_rect: egui::Rect, t: &theme::Tokens) {
    let rect = ui.max_rect();
    let full = full_cell(ui);
    let painter = gap_painter(ui, table_rect);
    let stroke = egui::Stroke::new(1.0, t.border);
    painter.hline(full.x_range(), rect.bottom(), stroke);
    painter.vline(full.right(), rect.y_range(), stroke);
}

/// Hover fill spanning the whole row; called from the first cell so the rest draws on top.
fn row_background(ui: &egui::Ui, table_rect: egui::Rect, t: &theme::Tokens) {
    let rect = ui.max_rect();
    let hovered = ui
        .ctx()
        .pointer_hover_pos()
        .is_some_and(|p| table_rect.contains(p) && p.y >= rect.top() && p.y < rect.bottom());
    if hovered {
        let row = egui::Rect::from_x_y_ranges(table_rect.x_range(), rect.y_range());
        gap_painter(ui, table_rect).rect_filled(row, 0.0, t.hover);
    }
}

/// One line of an `option_picker`: the value it writes (`None` is NULL), its label and color.
type PickOption = (Option<String>, String, egui::Color32);

/// The lines of the picker of a boolean cell: true, false and, when the column allows it, NULL.
fn bool_options(nullable: bool) -> Vec<PickOption> {
    let t = theme::tokens();
    let mut options = vec![
        (Some("true".to_string()), "true".to_string(), t.boolean),
        (Some("false".to_string()), "false".to_string(), t.foreground),
    ];
    if nullable {
        options.push((None, "NULL".into(), t.subtle));
    }
    options
}

/// The lines of the picker of an ENUM cell: its values, in the type's order, and NULL when the
/// column allows it.
fn enum_options(values: &[String], nullable: bool) -> Vec<PickOption> {
    let t = theme::tokens();
    let mut options: Vec<PickOption> = values
        .iter()
        .map(|v| (Some(v.clone()), v.clone(), t.foreground))
        .collect();
    if nullable {
        options.push((None, "NULL".into(), t.subtle));
    }
    options
}

/// Dropdown under a boolean or ENUM cell to pick one of `options`. Writes nothing by itself:
/// returns `Commit` for the chosen value (or `CancelEdit` for Esc, a click outside, or the
/// value the cell already has).
fn option_picker(
    ui: &egui::Ui,
    key: (u64, usize, usize),
    cell: egui::Rect,
    current: Option<&str>,
    options: &[PickOption],
) -> Option<GridEvent> {
    let t = theme::tokens();
    let (_, ri, ci) = key;
    let hl_id = egui::Id::new(("option_pick", key));
    let mut hl: usize = ui.data(|d| d.get_temp(hl_id)).unwrap_or_else(|| {
        options
            .iter()
            .position(|(v, ..)| v.as_deref() == current)
            .unwrap_or(0)
    });
    let (up, down, enter, esc) = ui.ctx().input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, Key::Enter),
            i.consume_key(egui::Modifiers::NONE, Key::Escape),
        )
    });
    if up {
        hl = hl.saturating_sub(1);
    }
    if down {
        hl = (hl + 1).min(options.len() - 1);
    }
    let choose = |i: usize| {
        let (v, ..) = &options[i];
        if v.as_deref() == current {
            GridEvent::CancelEdit
        } else {
            GridEvent::Commit(ri, ci, v.clone())
        }
    };
    let mut event = if esc {
        Some(GridEvent::CancelEdit)
    } else if enter {
        Some(choose(hl))
    } else {
        None
    };

    let area = egui::Area::new(hl_id.with("area"))
        .order(egui::Order::Foreground)
        .fixed_pos(cell.left_bottom() + egui::vec2(0.0, 2.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(t.card)
                .stroke(egui::Stroke::new(1.0, t.border))
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let width = cell.width().max(160.0);
                    for (i, (v, label, color)) in options.iter().enumerate() {
                        let (rect, r) = ui.allocate_exact_size(egui::vec2(width, 24.0), Sense::click());
                        if r.hovered() {
                            hl = i;
                        }
                        if r.clicked() {
                            event = Some(choose(i));
                        }
                        let p = ui.painter();
                        if i == hl {
                            p.rect_filled(rect, 0.0, t.selection);
                        }
                        if v.as_deref() == current {
                            p.text(
                                rect.left_center() + egui::vec2(8.0, 0.0),
                                egui::Align2::LEFT_CENTER,
                                icon::CHECK,
                                FontId::new(12.0, theme::icon_family()),
                                t.lime,
                            );
                        }
                        p.text(
                            rect.left_center() + egui::vec2(28.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            label,
                            FontId::monospace(12.0),
                            *color,
                        );
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("↑↓ Enter picks · Esc cancels")
                            .size(10.0)
                            .color(t.subtle),
                    );
                });
        });
    if event.is_none() && area.response.clicked_elsewhere() {
        event = Some(GridEvent::CancelEdit);
    }
    if event.is_some() {
        ui.data_mut(|d| d.remove::<usize>(hl_id));
    } else {
        ui.data_mut(|d| d.insert_temp(hl_id, hl));
    }
    event
}

/// Display state for the grid, owned by the caller.
pub struct GridOpts<'a> {
    pub can_edit: bool,
    pub selected: Option<(usize, usize)>,
    pub scroll_to: Option<usize>,
    /// Sort columns, the main one first.
    pub order: &'a [(String, bool)],
    /// Index of the first shown row in the full table (page * page size).
    pub row_offset: usize,
    /// Widths the user gave columns (by name), when this grid remembers them (table tabs).
    pub widths: Option<&'a std::collections::HashMap<String, f32>>,
    /// Boolean or ENUM cell with its value dropdown open.
    pub option_pick: Option<(usize, usize)>,
    /// Kind of each column (computed once per frame by the caller).
    pub kinds: &'a [ValueKind],
    /// Cells with an unsaved edit, and rows marked for deletion.
    pub changed: Option<&'a HashSet<(usize, usize)>>,
    /// Cells saved inside the open transaction (not committed yet).
    pub uncommitted: Option<&'a HashSet<(usize, usize)>>,
    pub deleted: Option<&'a HashSet<usize>>,
    /// Table tabs: offer "filter by this value" in the cell menu.
    pub can_filter: bool,
    /// Range selection, when more than one cell is selected.
    pub range: Option<&'a CellRange>,
    /// Columns to show, in order (indices into the result).
    pub view: &'a [usize],
    /// How many of `view`'s first columns stay put when scrolling sideways.
    pub pinned: usize,
    /// Cells matching "find in result", and the one it is on.
    pub found: Option<&'a HashSet<(usize, usize)>>,
    pub found_current: Option<(usize, usize)>,
    /// Foreign keys elsewhere pointing at this table, offered in the cell menu.
    pub incoming: &'a [IncomingFk],
    /// Columns with a "Filter values" condition: a funnel in their header.
    pub filtered: &'a [String],
}

/// A cell of a pinned column (or the row-number gutter, `col: None`) as drawn this frame. The
/// caller repaints these over the left edge when the grid is scrolled sideways.
pub struct Frozen {
    pub rect: egui::Rect,
    /// Visible part of the table (rows under the header are clipped).
    pub clip: egui::Rect,
    pub text: String,
    pub font: FontId,
    pub color: egui::Color32,
    pub bg: egui::Color32,
    pub right_align: bool,
    /// `None` for the header.
    pub row: Option<usize>,
    pub col: Option<usize>,
}

/// Result grid. Returns at most one user action per frame.
pub fn grid(
    ui: &mut egui::Ui,
    id: u64,
    res: &QueryResult,
    cols: &[ColumnInfo],
    mut edit: Option<&mut Edit>,
    opts: &GridOpts,
    frozen: &mut Vec<Frozen>,
) -> Option<GridEvent> {
    let GridOpts {
        can_edit,
        selected,
        scroll_to,
        order,
        row_offset,
        option_pick,
        widths,
        kinds,
        changed,
        uncommitted,
        deleted,
        range,
        view,
        pinned,
        found,
        found_current,
        filtered,
        ..
    } = *opts;
    let in_range = |ri: usize, ci: usize| range.is_some_and(|r| r.contains(ri, ci));
    // The selected cell's value (NULL and empty excluded): the same value elsewhere in its
    // column is lit, to spot repeats. Not while a range is selected.
    let same: Option<(usize, &str)> = selected
        .filter(|_| range.is_none())
        .and_then(|(r, c)| Some((c, res.rows.get(r)?.get(c)?.as_deref()?)))
        .filter(|(_, v)| !v.is_empty());
    // Dragging across cells selects a range: set when a drag starts on a cell, until release.
    let drag_id = egui::Id::new(("grid_drag", id));
    let dragging = ui.ctx().data(|d| d.get_temp::<bool>(drag_id)).unwrap_or(false);
    if dragging && !ui.input(|i| i.pointer.primary_down()) {
        ui.ctx().data_mut(|d| d.remove::<bool>(drag_id));
    }
    let mut event = None;
    let t = theme::tokens();
    let cell_font = FontId::monospace(cell_font_size());

    let info = column_info(res, cols);
    let sample = &res.rows[..res.rows.len().min(SAMPLE_ROWS)];

    ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);
    // The last column takes what's left, but the table adds a column gap on top: without this
    // it ends a gap's width past the visible area and right-aligned numbers get cut.
    let gap = ui.spacing().item_spacing.x;
    ui.set_max_width(ui.available_width() - gap);
    let table_rect = ui.available_rect_before_wrap();
    // Header band painted once across the full width, under the header cells.
    let header_band =
        egui::Rect::from_min_size(table_rect.min, egui::vec2(table_rect.width(), HEADER_HEIGHT));
    ui.painter().rect_filled(header_band, 0.0, t.card);
    ui.painter().hline(
        header_band.x_range(),
        header_band.bottom() - 0.5,
        egui::Stroke::new(1.0, t.border),
    );
    let mut table = TableBuilder::new(ui)
        // Column widths are stored by position: a different set of columns starts afresh.
        .id_salt(("grid", id, view))
        .striped(false)
        .resizable(true)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center));
    if let Some(row) = scroll_to {
        table = table.scroll_to_row(row, None);
    }
    // Row-number gutter sized to the largest number on the page.
    let digits = (row_offset + res.rows.len()).max(1).to_string().len() as f32;
    table = table.column(Column::exact((digits * char_width() + 12.0).max(32.0)));
    let last = view.len().saturating_sub(1);
    for (vi, &ci) in view.iter().enumerate() {
        let name = &res.columns[ci];
        let ty = info[ci].map(|c| c.data_type.as_str());
        // A width the user dragged to wins over the one worked out from the content.
        let w = widths.and_then(|w| w.get(name).copied()).unwrap_or_else(|| {
            column_width(
                name,
                ty,
                sample.iter().map(|r| r[ci].as_ref().map(|v| v.chars().count())),
            )
        });
        table = table.column(if vi == last {
            // The last column absorbs the remaining width so the grid fills the panel.
            Column::remainder().at_least(w).clip(true)
        } else {
            Column::initial(w).at_least(40.0).clip(true)
        });
    }

    // Widths as drawn this frame, to notice a column being dragged (the last one just fills up).
    let mut drawn: Vec<(&str, f32)> = Vec::new();
    table
        .header(HEADER_HEIGHT, |mut header| {
            header.col(|ui| {
                cell_chrome(ui, table_rect, t);
                if pinned > 0 {
                    frozen.push(Frozen {
                        rect: full_cell(ui),
                        clip: ui.clip_rect(),
                        text: String::new(),
                        font: cell_font.clone(),
                        color: t.subtle,
                        bg: t.card,
                        right_align: false,
                        row: None,
                        col: None,
                    });
                }
            });
            for (vi, &ci) in view.iter().enumerate() {
                let name = &res.columns[ci];
                header.col(|ui| {
                    cell_chrome(ui, table_rect, t);
                    if vi != last {
                        drawn.push((name.as_str(), ui.max_rect().width()));
                    }
                    if vi < pinned {
                        frozen.push(Frozen {
                            rect: full_cell(ui),
                            clip: ui.clip_rect(),
                            text: name.clone(),
                            font: egui::TextStyle::Body.resolve(ui.style()),
                            color: t.foreground,
                            bg: t.card,
                            right_align: false,
                            row: None,
                            col: Some(ci),
                        });
                    }
                    let c = info[ci];
                    if c.is_some_and(|c| c.pk) {
                        ui.label(theme::icon(icon::KEY).color(t.warning).size(12.0));
                    }
                    let r = ui.add(
                        egui::Label::new(RichText::new(name).color(t.foreground))
                            .truncate()
                            .sense(Sense::click()),
                    );
                    let place = order.iter().position(|(c, _)| c == name);
                    if let Some(pos) = place {
                        let a = if order[pos].1 {
                            icon::ARROW_UP
                        } else {
                            icon::ARROW_DOWN
                        };
                        ui.label(theme::icon(a).color(t.lime).size(12.0));
                        // Which of several sort columns this is.
                        if order.len() > 1 {
                            ui.label(RichText::new((pos + 1).to_string()).color(t.lime).size(10.0));
                        }
                    }
                    if filtered.contains(name)
                        && ui
                            .add(
                                egui::Label::new(theme::icon(icon::FUNNEL).color(t.lime).size(12.0))
                                    .sense(Sense::click()),
                            )
                            .on_hover_text("Filtered by values: click to change")
                            .clicked()
                    {
                        event = Some(GridEvent::FilterValues(ci));
                    }
                    // Type sits on the right, dimmed; it is the first thing to go when the column is narrow.
                    if let Some(c) = c {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(&c.data_type).size(11.0).color(t.subtle))
                                    .truncate(),
                            );
                        });
                    }
                    let r = r.on_hover_text(match c {
                        Some(c) => format!(
                            "{name}: {}{}\nClick sorts; Shift+click adds a sort column",
                            c.data_type,
                            if c.pk { " (primary key)" } else { "" }
                        ),
                        None => name.clone(),
                    });
                    if can_edit && r.clicked() {
                        // Shift adds the column to the sort instead of replacing it.
                        event = Some(if ui.input(|i| i.modifiers.shift) {
                            GridEvent::SortAlso(name.clone())
                        } else {
                            GridEvent::Sort(name.clone())
                        });
                    }
                    r.context_menu(|ui| {
                        if let Some(e) = header_menu(ui, res, opts, vi, ci) {
                            event = Some(e);
                        }
                    });
                });
            }
        })
        .body(|mut body| {
            // The cells take the pointer, so the table can't see which row it is over: the cell
            // under it says so (for the next frame) and the row is highlighted from that.
            let ctx = body.ui_mut().ctx().clone();
            let hover_id = egui::Id::new(("grid_hover_row", id));
            let hovered_row: Option<usize> = ctx.data_mut(|d| d.remove_temp(hover_id));
            body.rows(row_height(), res.rows.len(), |mut row| {
                let ri = row.index();
                if hovered_row == Some(ri) {
                    row.set_hovered(true);
                }
                row.col(|ui| {
                    row_background(ui, table_rect, t);
                    cell_chrome(ui, table_rect, t);
                    ui.with_layout(
                        Layout::centered_and_justified(egui::Direction::LeftToRight),
                        |ui| {
                            ui.label(
                                RichText::new((row_offset + ri + 1).to_string())
                                    .font(cell_font.clone())
                                    .color(t.subtle),
                            );
                        },
                    );
                    if pinned > 0 {
                        frozen.push(Frozen {
                            rect: full_cell(ui),
                            clip: ui.clip_rect(),
                            text: (row_offset + ri + 1).to_string(),
                            font: cell_font.clone(),
                            color: t.subtle,
                            bg: t.background,
                            right_align: false,
                            row: Some(ri),
                            col: None,
                        });
                    }
                });
                for (vi, &ci) in view.iter().enumerate() {
                    let kind = kinds[ci];
                    row.col(|ui| {
                        cell_chrome(ui, table_rect, t);
                        if let Some(e) = edit.as_deref_mut().filter(|e| e.row == ri && e.col == ci) {
                            let r = ui.add(
                                TextEdit::singleline(&mut e.text)
                                    .font(cell_font.clone())
                                    .margin(egui::Margin::symmetric(2, 0))
                                    .desired_width(f32::INFINITY),
                            );
                            if !e.focused {
                                r.request_focus();
                                e.focused = true;
                            }
                            if ui.input(|i| i.key_pressed(Key::Escape)) {
                                event = Some(GridEvent::CancelEdit);
                            } else if r.lost_focus() {
                                // Enter or clicking elsewhere both confirm, if something changed.
                                event = Some(if e.text == e.original {
                                    GridEvent::CancelEdit
                                } else {
                                    GridEvent::Commit(ri, ci, Some(e.text.clone()))
                                });
                            }
                            return;
                        }
                        let row_deleted = deleted.is_some_and(|d| d.contains(&ri));
                        let is_selected = selected == Some((ri, ci));
                        let fill = cell_fill(
                            t,
                            CellState {
                                selected: is_selected,
                                in_range: in_range(ri, ci),
                                found: found
                                    .is_some_and(|f| f.contains(&(ri, ci)))
                                    .then_some(found_current == Some((ri, ci))),
                                deleted: row_deleted,
                                changed: changed.is_some_and(|c| c.contains(&(ri, ci))),
                                uncommitted: uncommitted.is_some_and(|c| c.contains(&(ri, ci))),
                                same_value: same.is_some_and(|(c, v)| {
                                    c == ci
                                        && selected != Some((ri, ci))
                                        && res.rows[ri][ci].as_deref() == Some(v)
                                }),
                            },
                        );
                        if let Some(fill) = fill {
                            let rect = full_cell(ui);
                            let painter = gap_painter(ui, table_rect);
                            painter.rect_filled(rect, 0.0, fill);
                            if is_selected {
                                painter.rect_stroke(
                                    rect.shrink(0.5),
                                    0.0,
                                    egui::Stroke::new(1.0, t.ring),
                                    egui::StrokeKind::Inside,
                                );
                            }
                        }
                        let val = &res.rows[ri][ci];
                        // Foreign keys read as links: Ctrl+click opens the referenced row.
                        let fk = info[ci]
                            .and_then(|c| c.references.as_ref())
                            .filter(|_| val.is_some());
                        let text = match val {
                            None => RichText::new("NULL")
                                .font(cell_font.clone())
                                .color(t.subtle)
                                .italics(),
                            Some(v) => {
                                let color = match kind {
                                    _ if fk.is_some() => t.info,
                                    ValueKind::Number => t.number,
                                    ValueKind::Bool => t.boolean,
                                    ValueKind::Date => t.date,
                                    ValueKind::Text => t.foreground,
                                };
                                // Postgres sends booleans as t/f and MySQL as 1/0; show them as words.
                                let shown = display_value(kind, v);
                                let color = if shown == "false" {
                                    t.muted_foreground
                                } else {
                                    color
                                };
                                RichText::new(shown).font(cell_font.clone()).color(color)
                            }
                        };
                        if vi < pinned {
                            let bg = fill.unwrap_or(t.background);
                            let (shown, color) = match val {
                                None => ("NULL".to_string(), t.subtle),
                                Some(v) => (
                                    display_value(kind, v),
                                    match kind {
                                        ValueKind::Number => t.number,
                                        ValueKind::Bool => t.boolean,
                                        ValueKind::Date => t.date,
                                        ValueKind::Text => t.foreground,
                                    },
                                ),
                            };
                            frozen.push(Frozen {
                                rect: full_cell(ui),
                                clip: ui.clip_rect(),
                                text: shown,
                                font: cell_font.clone(),
                                color,
                                bg,
                                right_align: kind == ValueKind::Number,
                                row: Some(ri),
                                col: Some(ci),
                            });
                        }
                        // Rows going away read as struck through.
                        let text = if row_deleted {
                            text.strikethrough().color(t.subtle)
                        } else {
                            text
                        };
                        // The text only draws; the whole cell takes the clicks, double clicks, drags
                        // and the context menu (short values and NULLs left little to aim at).
                        let label = egui::Label::new(text).truncate().selectable(false);
                        if kind == ValueKind::Number {
                            // A little room before the column's edge, like the left-aligned text has.
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_space(NUMBER_PAD);
                                ui.add(label);
                            });
                        } else {
                            ui.add(label);
                        }
                        let cell = full_cell(ui);
                        let mut r =
                            ui.interact(cell, ui.id().with(("cell", ri, ci)), Sense::click_and_drag());
                        if r.contains_pointer() {
                            ui.ctx().data_mut(|d| d.insert_temp(hover_id, ri));
                        }
                        // A value cut with "…" shows whole under the pointer.
                        if r.hovered()
                            && let Some(v) = val
                        {
                            let width = ui
                                .painter()
                                .layout_no_wrap(v.to_string(), cell_font.clone(), t.foreground)
                                .size()
                                .x;
                            if width > cell.width() - 8.0 {
                                r = r.on_hover_text(egui::RichText::new(v.as_str()).monospace());
                            }
                        }
                        if option_pick == Some((ri, ci)) {
                            let nullable = info[ci].is_none_or(|c| c.nullable);
                            let cell = full_cell(ui);
                            let (options, current) = match info[ci] {
                                Some(c) if !c.enum_values.is_empty() => {
                                    (enum_options(&c.enum_values, nullable), val.as_deref())
                                }
                                _ => (
                                    bool_options(nullable),
                                    val.as_deref()
                                        .and_then(crate::sql::parse_bool)
                                        .map(|b| if b { "true" } else { "false" }),
                                ),
                            };
                            if let Some(e) = option_picker(ui, (id, ri, ci), cell, current, &options) {
                                event = Some(e);
                            }
                        }
                        let r = match fk {
                            Some(f) => r
                                .on_hover_text(format!(
                                    "Ctrl+click opens {}.{} ({} = this value)",
                                    f.schema, f.table, f.column
                                ))
                                .on_hover_cursor(egui::CursorIcon::PointingHand),
                            None => r,
                        };
                        let shift = ui.input(|i| i.modifiers.shift);
                        if fk.is_some() && r.clicked() && ui.input(|i| i.modifiers.command) {
                            event = Some(GridEvent::FollowFk(ri, ci));
                        // A click to select and a quick double click after it reach egui as a
                        // triple click: that one edits too.
                        } else if can_edit && (r.double_clicked() || r.triple_clicked()) {
                            event = Some(GridEvent::StartEdit(ri, ci));
                        } else if r.clicked() && shift {
                            event = Some(GridEvent::Extend(ri, ci));
                        } else if r.drag_started() {
                            ui.ctx().data_mut(|d| d.insert_temp(drag_id, true));
                            event = Some(if shift {
                                GridEvent::Extend(ri, ci)
                            } else {
                                GridEvent::Select(ri, ci)
                            });
                        } else if dragging && selected != Some((ri, ci)) && r.contains_pointer() {
                            event = Some(GridEvent::Extend(ri, ci));
                        } else if r.clicked() || (r.secondary_clicked() && !in_range(ri, ci)) {
                            // Right-clicking inside the range keeps it, for "Copy selection".
                            event = Some(GridEvent::Select(ri, ci));
                        }
                        r.context_menu(|ui| {
                            if let Some(e) = cell_menu(ui, res, opts, (ri, ci), fk, row_deleted) {
                                event = Some(e);
                            }
                        });
                    });
                }
            });
            // The pointer is over another row now (rows shifted under a still pointer): one more
            // frame shows it. (Leaving the grid comes with pointer events, which redraw anyway.)
            let now = ctx.data(|d| d.get_temp::<usize>(hover_id));
            if now.is_some() && now != hovered_row {
                ctx.request_repaint();
            }
        });
    // A column dragged to a new width is reported once the pointer is let go (not on every frame
    // of the drag), so the caller can remember it.
    if widths.is_some() {
        let ctx = ui.ctx().clone();
        let seen_id = egui::Id::new(("grid_widths_seen", id));
        let moved_id = egui::Id::new(("grid_widths_moved", id));
        // What was drawn last frame: the column names as one hash, and the widths. Compared
        // without building any strings, since this runs every frame.
        let names = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            drawn.iter().for_each(|(n, _)| n.hash(&mut h));
            h.finish()
        };
        let seen: Option<(u64, Vec<f32>)> = ctx.data(|d| d.get_temp(seen_id));
        let changed = seen.as_ref().is_some_and(|(n, w)| {
            *n == names && w.len() == drawn.len() && w.iter().zip(&drawn).any(|(a, b)| (a - b.1).abs() > 0.5)
        });
        let pending = changed || ctx.data(|d| d.get_temp::<bool>(moved_id)).unwrap_or(false);
        if seen.as_ref().is_none_or(|(n, w)| {
            *n != names || w.len() != drawn.len() || w.iter().zip(&drawn).any(|(a, b)| a != &b.1)
        }) {
            ctx.data_mut(|d| {
                d.insert_temp(
                    seen_id,
                    (names, drawn.iter().map(|(_, w)| *w).collect::<Vec<f32>>()),
                )
            });
        }
        if pending && ctx.input(|i| i.pointer.any_down()) {
            ctx.data_mut(|d| d.insert_temp(moved_id, true));
        } else if pending {
            ctx.data_mut(|d| d.remove::<bool>(moved_id));
            if event.is_none() {
                event = Some(GridEvent::ColumnWidths(
                    drawn.into_iter().map(|(n, w)| (n.to_string(), w)).collect(),
                ));
            }
        }
    }
    event
}

/// A column header's context menu.
fn header_menu(
    ui: &mut egui::Ui,
    res: &QueryResult,
    opts: &GridOpts,
    vi: usize,
    ci: usize,
) -> Option<GridEvent> {
    let GridOpts {
        can_filter,
        view,
        pinned,
        ..
    } = *opts;
    let name = &res.columns[ci];
    let mut event = None;
    if can_filter
        && ui
            .button("Filter values…")
            .on_hover_text("Check the values to keep, like Excel's filter")
            .clicked()
    {
        event = Some(GridEvent::FilterValues(ci));
        ui.close();
    }
    if ui.button("Column statistics").clicked() {
        event = Some(GridEvent::ColumnStats(ci));
        ui.close();
    }
    ui.separator();
    let pin = if vi < pinned { "Unpin" } else { "Pin to the left" };
    if ui
        .button(pin)
        .on_hover_text("Pinned columns stay visible when scrolling sideways")
        .clicked()
    {
        event = Some(GridEvent::TogglePin(ci));
        ui.close();
    }
    if view.len() > 1 && ui.button("Hide column").clicked() {
        event = Some(GridEvent::ToggleColumn(ci));
        ui.close();
    }
    let hidden = res.columns.len() - view.len();
    ui.menu_button(
        format!("Visible columns ({}/{})", view.len(), res.columns.len()),
        |ui| {
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for (i, n) in res.columns.iter().enumerate() {
                    let mut on = view.contains(&i);
                    // The last visible one can't go: something must stay clickable.
                    let can = !on || view.len() > 1;
                    if ui.add_enabled(can, egui::Checkbox::new(&mut on, n)).changed() {
                        event = Some(GridEvent::ToggleColumn(i));
                    }
                }
            });
        },
    );
    if hidden > 0 && ui.button(format!("Show all ({hidden} hidden)")).clicked() {
        event = Some(GridEvent::ShowAllColumns);
        ui.close();
    }
    ui.separator();
    if ui.button("Copy name").clicked() {
        ui.ctx().copy_text(name.clone());
        ui.close();
    }
    if ui
        .button("Copy values as (…) list")
        .on_hover_text("The distinct values of the selection, or of the whole column: for IN (…)")
        .clicked()
    {
        event = Some(GridEvent::CopyInList(ci));
        ui.close();
    }
    event
}

/// A cell's context menu. `fk` is where its value points, if it is a foreign key.
fn cell_menu(
    ui: &mut egui::Ui,
    res: &QueryResult,
    opts: &GridOpts,
    (ri, ci): (usize, usize),
    fk: Option<&FkRef>,
    row_deleted: bool,
) -> Option<GridEvent> {
    let GridOpts {
        can_edit,
        can_filter,
        range,
        incoming,
        ..
    } = *opts;
    let val = &res.rows[ri][ci];
    let mut event = None;
    let open = if can_edit {
        "Open value to edit…"
    } else {
        "Open value…"
    };
    if ui
        .add(egui::Button::new(open).shortcut_text("Shift+Enter"))
        .on_hover_text("Large editor: formatted and validated JSON, long texts")
        .clicked()
    {
        event = Some(GridEvent::OpenValue(ri, ci));
        ui.close();
    }
    ui.separator();
    if let Some(f) = fk
        && ui
            .button(format!("Open {} ({} = this value)", f.table, f.column))
            .clicked()
    {
        event = Some(GridEvent::FollowFk(ri, ci));
        ui.close();
    }
    // Other tables' rows that reference this one, by the column they point at.
    let pointing: Vec<(usize, &IncomingFk)> = incoming
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            res.columns
                .iter()
                .position(|c| *c == f.target)
                .is_some_and(|i| res.rows[ri][i].is_some())
        })
        .collect();
    if !pointing.is_empty() {
        ui.menu_button("Rows pointing to this one", |ui| {
            for (i, f) in &pointing {
                let label = format!("{}.{}  ({} = {})", f.table, f.column, f.column, f.target);
                if ui.button(label).clicked() {
                    event = Some(GridEvent::FollowIncoming(ri, *i));
                    ui.close();
                }
            }
        });
        ui.separator();
    }
    if can_filter {
        let (eq, ne) = if val.is_some() {
            ("Filter: equal to this value", "Filter: different from this value")
        } else {
            ("Filter: is NULL", "Filter: is not NULL")
        };
        if ui.button(eq).clicked() {
            event = Some(GridEvent::Filter(ri, ci, FilterOp::Eq));
            ui.close();
        }
        if ui.button(ne).clicked() {
            event = Some(GridEvent::Filter(ri, ci, FilterOp::Ne));
            ui.close();
        }
        ui.separator();
    }
    if let Some(sel) = range.filter(|_| can_edit) {
        let cells = (sel.r1 - sel.r0 + 1) * sel.cols.len();
        if ui.button(format!("Set value in the {cells} cells…")).clicked() {
            event = Some(GridEvent::AskFill);
            ui.close();
        }
        if ui.button("Set NULL in the selection").clicked() {
            event = Some(GridEvent::FillSelection(None));
            ui.close();
        }
        if sel.r1 > sel.r0
            && ui
                .add(egui::Button::new("Fill down").shortcut_text("Ctrl+D"))
                .on_hover_text("Each column gets the value of the first selected row")
                .clicked()
        {
            event = Some(GridEvent::FillDown);
            ui.close();
        }
        let rows = sel.r1 - sel.r0 + 1;
        if ui
            .add(egui::Button::new(format!("Delete {rows} row(s)")).shortcut_text("Delete"))
            .clicked()
        {
            event = Some(GridEvent::DeleteRows);
            ui.close();
        }
        ui.separator();
    }
    if can_edit && can_filter {
        // A bulk UPDATE of this column (table tabs): opens a form, then a preview.
        if ui
            .button(format!("Set {} with an expression…", res.columns[ci]))
            .on_hover_text(
                "UPDATE with a SQL expression (price * 1.1) on the selected rows, previewed before it runs",
            )
            .clicked()
        {
            event = Some(GridEvent::AskBulk(ci));
            ui.close();
        }
        ui.separator();
    }
    if range.is_some() {
        if ui
            .button("Copy selection")
            .on_hover_text("Ctrl+C · tab-separated text, pastes into Excel")
            .clicked()
        {
            event = Some(GridEvent::CopyCells { headers: false });
            ui.close();
        }
        if ui.button("Copy selection with header").clicked() {
            event = Some(GridEvent::CopyCells { headers: true });
            ui.close();
        }
        ui.separator();
    }
    let md = if range.is_some() {
        "Copy selection as Markdown"
    } else {
        "Copy result as Markdown"
    };
    if ui
        .button(md)
        .on_hover_text("A table ready to paste into an issue, docs or chat")
        .clicked()
    {
        event = Some(GridEvent::CopyMarkdown);
        ui.close();
    }
    if ui.button("Copy value").clicked() {
        ui.ctx().copy_text(val.clone().unwrap_or_default());
        ui.close();
    }
    if ui.button("Copy row as JSON").clicked() {
        event = Some(GridEvent::Copy(ri, CopyAs::Json));
        ui.close();
    }
    if ui.button("Copy row as INSERT").clicked() {
        event = Some(GridEvent::Copy(ri, CopyAs::Insert));
        ui.close();
    }
    if can_edit {
        // The insert form belongs to table tabs.
        if can_filter
            && ui
                .button("Duplicate row")
                .on_hover_text("Opens the insert form filled with this row")
                .clicked()
        {
            event = Some(GridEvent::DuplicateRow(ri));
            ui.close();
        }
        if ui.button("Set NULL").clicked() {
            event = Some(GridEvent::Commit(ri, ci, None));
            ui.close();
        }
        let label = if row_deleted { "Restore row" } else { "Delete row" };
        if ui.button(label).clicked() {
            event = Some(GridEvent::DeleteRow(ri));
            ui.close();
        }
    }
    event
}

/// What a cell is showing besides its value.
#[derive(Clone, Copy)]
struct CellState {
    selected: bool,
    in_range: bool,
    /// Matches "find in result"; `true` for the match it is on.
    found: Option<bool>,
    deleted: bool,
    changed: bool,
    /// Saved in the open transaction, not committed yet.
    uncommitted: bool,
    /// The same value as the selected cell, in its column.
    same_value: bool,
}

/// The cell's background, when not the plain one. The strongest state wins.
fn cell_fill(t: &theme::Tokens, c: CellState) -> Option<egui::Color32> {
    let mix = |color, pct| theme::mix(color, pct, t.background);
    if c.selected {
        Some(t.selection)
    } else if c.in_range {
        Some(mix(t.selection, 0.6))
    } else if let Some(current) = c.found {
        Some(mix(t.warning, if current { 0.45 } else { 0.22 }))
    } else if c.deleted {
        Some(mix(t.destructive, 0.14))
    } else if c.changed {
        Some(mix(t.warning, 0.18))
    } else if c.uncommitted {
        Some(mix(t.lime, 0.2))
    } else if c.same_value {
        Some(mix(t.selection, 0.45))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_repeated_value_is_lit_below_the_stronger_states() {
        let t = super::theme::tokens();
        let state = |same_value, selected, changed| CellState {
            selected,
            in_range: false,
            found: None,
            deleted: false,
            changed,
            uncommitted: false,
            same_value,
        };
        assert!(cell_fill(t, state(true, false, false)).is_some());
        assert!(cell_fill(t, state(false, false, false)).is_none());
        assert_eq!(
            cell_fill(t, state(true, true, false)),
            Some(t.selection),
            "selection wins"
        );
        assert_ne!(
            cell_fill(t, state(true, false, true)),
            cell_fill(t, state(true, false, false)),
            "an unsaved edit still shows"
        );
    }

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn widths_follow_content() {
        let narrow = column_width("id", Some("int4"), [Some(3)].into_iter());
        let wide = column_width("email", Some("text"), [Some(40)].into_iter());
        assert_eq!(narrow, MIN_COL_WIDTH);
        assert!(wide > 300.0, "{wide}");
        // The type shouldn't get cut: "created_at" + "timestamp without time zone" fits.
        assert!(
            column_width(
                "created_at",
                Some("timestamp without time zone"),
                std::iter::empty()
            ) > 300.0
        );
        assert_eq!(column_width("x", None, [Some(500)].into_iter()), MAX_COL_WIDTH);
    }

    fn result() -> QueryResult {
        QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: vec![
                vec![Some("1".into()), Some("anna".into())],
                vec![Some("2".into()), None],
            ],
            ..Default::default()
        }
    }

    #[test]
    fn renders_values_and_null() {
        let res = result();
        let harness = Harness::new_ui(|ui| {
            grid(
                ui,
                1,
                &res,
                &[],
                None,
                &GridOpts {
                    can_edit: false,
                    selected: None,
                    scroll_to: None,
                    order: &[],
                    row_offset: 0,
                    option_pick: None,
                    widths: None,
                    kinds: &[ValueKind::Number, ValueKind::Text],
                    changed: None,
                    uncommitted: None,
                    deleted: None,
                    can_filter: false,
                    range: None,
                    view: &[0, 1],
                    pinned: 0,
                    found: None,
                    found_current: None,
                    incoming: &[],
                    filtered: &[],
                },
                &mut Vec::new(),
            );
        });
        harness.get_by_label("anna");
        harness.get_by_label("NULL");
        harness.get_by_label("id");
    }

    #[test]
    fn header_click_requests_sort() {
        let res = result();
        let mut got = None;
        let mut harness = Harness::new_ui(|ui| {
            if let Some(e) = grid(
                ui,
                1,
                &res,
                &[],
                None,
                &GridOpts {
                    can_edit: true,
                    selected: None,
                    scroll_to: None,
                    order: &[],
                    row_offset: 0,
                    option_pick: None,
                    widths: None,
                    kinds: &[ValueKind::Number, ValueKind::Text],
                    changed: None,
                    uncommitted: None,
                    deleted: None,
                    can_filter: false,
                    range: None,
                    view: &[0, 1],
                    pinned: 0,
                    found: None,
                    found_current: None,
                    incoming: &[],
                    filtered: &[],
                },
                &mut Vec::new(),
            ) {
                got = Some(e);
            }
        });
        harness.get_by_label("name").click();
        harness.run();
        drop(harness);
        assert!(matches!(got, Some(GridEvent::Sort(c)) if c == "name"));
    }
}
