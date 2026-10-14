use eframe::egui::{self, Align, FontId, Key, Layout, RichText, Sense, TextEdit};
use std::collections::HashSet;

use egui_extras::{Column, TableBuilder};

use super::{icon, theme};
use crate::app::CopyAs;
use crate::app::{Edit, GridEvent};
use crate::db::{ColumnInfo, IncomingFk, QueryResult};
use crate::sql::FilterOp;
use crate::values::{CellRange, SAMPLE_ROWS, ValueKind, column_info};

const HEADER_HEIGHT: f32 = 28.0;
/// Row height for the grid font (a preference): 24 at the default 12.
fn row_height() -> f32 {
    (crate::settings::grid_font() + 12.0).round()
}

fn cell_font_size() -> f32 {
    crate::settings::grid_font()
}
/// Rough advance of one character of the cell font, for sizing columns to their content.
/// Width of one monospace char at the grid font.
fn char_width() -> f32 {
    7.3 * crate::settings::grid_font() / 12.0
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
        // "Números com vírgula decimal": display only; editing and copying keep the real value.
        (ValueKind::Number, v) if crate::settings::decimal_comma() => v.replace('.', ","),
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

/// Dropdown under a boolean cell to pick true / false / NULL. Writes nothing by itself:
/// returns `Commit` for the chosen value (or `CancelEdit` for Esc, a click outside, or the
/// value the cell already has).
fn bool_picker(
    ui: &egui::Ui,
    key: (u64, usize, usize),
    cell: egui::Rect,
    current: Option<bool>,
    nullable: bool,
) -> Option<GridEvent> {
    let t = theme::tokens();
    let (_, ri, ci) = key;
    let mut options: Vec<(Option<bool>, &str)> = vec![(Some(true), "true"), (Some(false), "false")];
    if nullable {
        options.push((None, "NULL"));
    }
    let hl_id = egui::Id::new(("bool_pick", key));
    let mut hl: usize = ui
        .data(|d| d.get_temp(hl_id))
        .unwrap_or_else(|| options.iter().position(|(v, _)| *v == current).unwrap_or(0));
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
        let (v, _) = options[i];
        if v == current {
            GridEvent::CancelEdit
        } else {
            GridEvent::Commit(ri, ci, v.map(|b| b.to_string()))
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
                    for (i, (v, label)) in options.iter().enumerate() {
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
                        if *v == current {
                            p.text(
                                rect.left_center() + egui::vec2(8.0, 0.0),
                                egui::Align2::LEFT_CENTER,
                                icon::CHECK,
                                FontId::new(12.0, theme::icon_family()),
                                t.lime,
                            );
                        }
                        let color = match v {
                            Some(true) => t.boolean,
                            Some(false) => t.foreground,
                            None => t.subtle,
                        };
                        p.text(
                            rect.left_center() + egui::vec2(28.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            label,
                            FontId::monospace(12.0),
                            color,
                        );
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("↑↓ Enter escolhe · Esc cancela")
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
    pub order: &'a Option<(String, bool)>,
    /// Index of the first shown row in the full table (page * page size).
    pub row_offset: usize,
    /// Boolean cell with its value dropdown open.
    pub bool_pick: Option<(usize, usize)>,
    /// Kind of each column (computed once per frame by the caller).
    pub kinds: &'a [ValueKind],
    /// Cells with an unsaved edit, and rows marked for deletion.
    pub changed: Option<&'a HashSet<(usize, usize)>>,
    pub deleted: Option<&'a HashSet<usize>>,
    /// Table tabs: offer "filter by this value" in the cell menu.
    pub can_filter: bool,
    /// Range selection, when more than one cell is selected.
    pub range: Option<&'a CellRange>,
    /// Columns to show, in order (indices into the result).
    pub view: &'a [usize],
    /// How many of `view`'s first columns stay put when scrolling sideways.
    pub pinned: usize,
    /// Cells matching "buscar no resultado", and the one it is on.
    pub found: Option<&'a HashSet<(usize, usize)>>,
    pub found_current: Option<(usize, usize)>,
    /// Foreign keys elsewhere pointing at this table, offered in the cell menu.
    pub incoming: &'a [IncomingFk],
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
        bool_pick,
        kinds,
        changed,
        deleted,
        can_filter,
        range,
        view,
        pinned,
        found,
        found_current,
        incoming,
    } = *opts;
    let in_range = |ri: usize, ci: usize| range.is_some_and(|r| r.contains(ri, ci));
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
        let w = column_width(
            name,
            ty,
            sample.iter().map(|r| r[ci].as_ref().map(|v| v.chars().count())),
        );
        table = table.column(if vi == last {
            // The last column absorbs the remaining width so the grid fills the panel.
            Column::remainder().at_least(w).clip(true)
        } else {
            Column::initial(w).at_least(40.0).clip(true)
        });
    }

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
                    let arrow = match order {
                        Some((c, true)) if c == name => Some(icon::ARROW_UP),
                        Some((c, false)) if c == name => Some(icon::ARROW_DOWN),
                        _ => None,
                    };
                    if let Some(a) = arrow {
                        ui.label(theme::icon(a).color(t.lime).size(12.0));
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
                            "{name}: {}{}",
                            c.data_type,
                            if c.pk { " (chave primária)" } else { "" }
                        ),
                        None => name.clone(),
                    });
                    if can_edit && r.clicked() {
                        event = Some(GridEvent::Sort(name.clone()));
                    }
                    r.context_menu(|ui| {
                        if ui.button("Estatísticas da coluna").clicked() {
                            event = Some(GridEvent::ColumnStats(ci));
                            ui.close();
                        }
                        ui.separator();
                        let pin = if vi < pinned {
                            "Desafixar"
                        } else {
                            "Fixar à esquerda"
                        };
                        if ui
                            .button(pin)
                            .on_hover_text("Colunas fixadas ficam visíveis ao rolar para o lado")
                            .clicked()
                        {
                            event = Some(GridEvent::TogglePin(ci));
                            ui.close();
                        }
                        if view.len() > 1 && ui.button("Esconder coluna").clicked() {
                            event = Some(GridEvent::ToggleColumn(ci));
                            ui.close();
                        }
                        let hidden = res.columns.len() - view.len();
                        ui.menu_button(
                            format!("Colunas visíveis ({}/{})", view.len(), res.columns.len()),
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
                        if hidden > 0 && ui.button(format!("Mostrar todas ({hidden} ocultas)")).clicked() {
                            event = Some(GridEvent::ShowAllColumns);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Copiar nome").clicked() {
                            ui.ctx().copy_text(name.clone());
                            ui.close();
                        }
                    });
                });
            }
        })
        .body(|body| {
            body.rows(row_height(), res.rows.len(), |mut row| {
                let ri = row.index();
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
                        if row_deleted {
                            gap_painter(ui, table_rect).rect_filled(
                                full_cell(ui),
                                0.0,
                                theme::mix(t.destructive, 0.14, t.background),
                            );
                        } else if changed.is_some_and(|c| c.contains(&(ri, ci))) {
                            gap_painter(ui, table_rect).rect_filled(
                                full_cell(ui),
                                0.0,
                                theme::mix(t.warning, 0.18, t.background),
                            );
                        }
                        let is_found = found.is_some_and(|f| f.contains(&(ri, ci)));
                        if is_found && selected != Some((ri, ci)) {
                            let strength = if found_current == Some((ri, ci)) {
                                0.45
                            } else {
                                0.22
                            };
                            gap_painter(ui, table_rect).rect_filled(
                                full_cell(ui),
                                0.0,
                                theme::mix(t.warning, strength, t.background),
                            );
                        }
                        if selected != Some((ri, ci)) && in_range(ri, ci) {
                            gap_painter(ui, table_rect).rect_filled(
                                full_cell(ui),
                                0.0,
                                theme::mix(t.selection, 0.6, t.background),
                            );
                        }
                        if selected == Some((ri, ci)) {
                            let rect = full_cell(ui);
                            let painter = gap_painter(ui, table_rect);
                            painter.rect_filled(rect, 0.0, t.selection);
                            painter.rect_stroke(
                                rect.shrink(0.5),
                                0.0,
                                egui::Stroke::new(1.0, t.ring),
                                egui::StrokeKind::Inside,
                            );
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
                            let bg = if selected == Some((ri, ci)) {
                                t.selection
                            } else if in_range(ri, ci) {
                                theme::mix(t.selection, 0.6, t.background)
                            } else if row_deleted {
                                theme::mix(t.destructive, 0.14, t.background)
                            } else if changed.is_some_and(|c| c.contains(&(ri, ci))) {
                                theme::mix(t.warning, 0.18, t.background)
                            } else {
                                t.background
                            };
                            let (shown, color) = match val {
                                None => ("NULL".to_string(), t.subtle),
                                Some(v) => (
                                    display_value(kind, v).to_string(),
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
                        let label = egui::Label::new(text).truncate().sense(Sense::click_and_drag());
                        let r = if kind == ValueKind::Number {
                            // A little room before the column's edge, like the left-aligned text has.
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_space(NUMBER_PAD);
                                ui.add(label)
                            })
                            .inner
                        } else {
                            ui.add(label)
                        };
                        if bool_pick == Some((ri, ci)) {
                            let current = val.as_deref().and_then(crate::sql::parse_bool);
                            let nullable = info[ci].is_none_or(|c| c.nullable);
                            let cell = full_cell(ui);
                            if let Some(e) = bool_picker(ui, (id, ri, ci), cell, current, nullable) {
                                event = Some(e);
                            }
                        }
                        let r = match fk {
                            Some(f) => r
                                .on_hover_text(format!(
                                    "Ctrl+clique abre {}.{} ({} = este valor)",
                                    f.schema, f.table, f.column
                                ))
                                .on_hover_cursor(egui::CursorIcon::PointingHand),
                            None => r,
                        };
                        let shift = ui.input(|i| i.modifiers.shift);
                        if fk.is_some() && r.clicked() && ui.input(|i| i.modifiers.command) {
                            event = Some(GridEvent::FollowFk(ri, ci));
                        } else if can_edit && r.double_clicked() {
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
                            // Right-clicking inside the range keeps it, for "Copiar seleção".
                            event = Some(GridEvent::Select(ri, ci));
                        }
                        r.context_menu(|ui| {
                            if let Some(f) = fk
                                && ui
                                    .button(format!("Abrir {} ({} = este valor)", f.table, f.column))
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
                                ui.menu_button("Linhas que apontam para esta", |ui| {
                                    for (i, f) in &pointing {
                                        let label = format!(
                                            "{}.{}  ({} = {})",
                                            f.table, f.column, f.column, f.target
                                        );
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
                                    ("Filtrar: igual a este valor", "Filtrar: diferente deste valor")
                                } else {
                                    ("Filtrar: é NULL", "Filtrar: não é NULL")
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
                            if range.is_some() {
                                if ui
                                    .button("Copiar seleção")
                                    .on_hover_text("Ctrl+C · texto separado por tabs, cola no Excel")
                                    .clicked()
                                {
                                    event = Some(GridEvent::CopyCells { headers: false });
                                    ui.close();
                                }
                                if ui.button("Copiar seleção com cabeçalho").clicked() {
                                    event = Some(GridEvent::CopyCells { headers: true });
                                    ui.close();
                                }
                                ui.separator();
                            }
                            if ui.button("Copiar valor").clicked() {
                                ui.ctx().copy_text(val.clone().unwrap_or_default());
                                ui.close();
                            }
                            if ui.button("Copiar linha como JSON").clicked() {
                                event = Some(GridEvent::Copy(ri, CopyAs::Json));
                                ui.close();
                            }
                            if ui.button("Copiar linha como INSERT").clicked() {
                                event = Some(GridEvent::Copy(ri, CopyAs::Insert));
                                ui.close();
                            }
                            if can_edit {
                                // The insert form belongs to table tabs.
                                if can_filter
                                    && ui
                                        .button("Duplicar linha")
                                        .on_hover_text(
                                            "Abre o formulário de inserção preenchido com esta linha",
                                        )
                                        .clicked()
                                {
                                    event = Some(GridEvent::DuplicateRow(ri));
                                    ui.close();
                                }
                                if ui.button("Definir NULL").clicked() {
                                    event = Some(GridEvent::Commit(ri, ci, None));
                                    ui.close();
                                }
                                let label = if row_deleted {
                                    "Restaurar linha"
                                } else {
                                    "Excluir linha"
                                };
                                if ui.button(label).clicked() {
                                    event = Some(GridEvent::DeleteRow(ri));
                                    ui.close();
                                }
                            }
                        });
                    });
                }
            });
        });
    event
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn widths_follow_content() {
        let narrow = column_width("id", Some("int4"), [Some(3)].into_iter());
        let wide = column_width("email", Some("text"), [Some(40)].into_iter());
        assert_eq!(narrow, MIN_COL_WIDTH);
        assert!(wide > 300.0, "{wide}");
        // The type shouldn't get cut: "criado_em" + "timestamp without time zone" fits.
        assert!(
            column_width(
                "criado_em",
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
                vec![Some("1".into()), Some("ana".into())],
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
                    order: &None,
                    row_offset: 0,
                    bool_pick: None,
                    kinds: &[ValueKind::Number, ValueKind::Text],
                    changed: None,
                    deleted: None,
                    can_filter: false,
                    range: None,
                    view: &[0, 1],
                    pinned: 0,
                    found: None,
                    found_current: None,
                    incoming: &[],
                },
                &mut Vec::new(),
            );
        });
        harness.get_by_label("ana");
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
                    order: &None,
                    row_offset: 0,
                    bool_pick: None,
                    kinds: &[ValueKind::Number, ValueKind::Text],
                    changed: None,
                    deleted: None,
                    can_filter: false,
                    range: None,
                    view: &[0, 1],
                    pinned: 0,
                    found: None,
                    found_current: None,
                    incoming: &[],
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
