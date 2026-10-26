//! Empty states for the main area: connection cards before connecting, shortcuts once connected.

use std::sync::Arc;

use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{self, Align2, FontId, Galley, RichText, Sense};

use super::{icon, palette::format_count, theme};
use crate::app::{App, ConnForm};
use crate::db::{ConnConfig, Kind};

const CARD_WIDTH: f32 = 440.0;

/// A key drawn as a small outlined cap, e.g. `Ctrl+P`.
fn keycap(ui: &mut egui::Ui, keys: &str) {
    let t = theme::tokens();
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, t.border))
        .fill(t.sidebar)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(
                RichText::new(keys)
                    .monospace()
                    .size(12.0)
                    .color(t.muted_foreground),
            )
        });
}

impl App {
    pub fn welcome(&mut self, ui: &mut egui::Ui) {
        let t = theme::tokens();
        let mut connect = None;
        let mut edit = None;
        let mut delete = None;
        let mut new_conn = false;
        let mut open_palette = false;
        let mut new_sql = false;

        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.16).max(24.0));
            ui.label(theme::icon(icon::DATABASE).size(36.0).color(t.accent_text));
            ui.add_space(4.0);
            ui.label(RichText::new("tabella").size(22.0).color(t.foreground));
            ui.label(
                RichText::new("Lightweight client for PostgreSQL, MySQL and SQLite")
                    .color(t.muted_foreground),
            );
            ui.add_space(24.0);

            match &self.session {
                None => {
                    for (group, indexes) in crate::db::by_group(&self.connections.saved) {
                        if !group.is_empty() {
                            ui.add_space(4.0);
                            ui.label(RichText::new(group).small().color(t.subtle));
                        }
                        for i in indexes {
                            let c = &self.connections.saved[i];
                            match connection_card(ui, c) {
                                Some(CardAction::Connect) => connect = Some(c.id.clone()),
                                Some(CardAction::Edit) => edit = Some(c.clone()),
                                Some(CardAction::Delete) => delete = Some(c.id.clone()),
                                None => {}
                            }
                            ui.add_space(6.0);
                        }
                    }
                    ui.add_space(8.0);
                    new_conn = ui
                        .add(theme::primary_button(Some(icon::PLUS), "New connection"))
                        .clicked();
                }
                Some(s) if !s.ready => {
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 160.0) / 2.0);
                        ui.spinner();
                        ui.label(
                            RichText::new(format!("Connecting to {}…", s.cfg.name)).color(t.muted_foreground),
                        );
                    });
                }
                Some(s) => {
                    let tables = s.tables.iter().filter(|t| !t.is_view).count();
                    ui.label(
                        RichText::new(format!(
                            "Connected to {} · {} tables",
                            s.cfg.name,
                            format_count(tables as i64)
                        ))
                        .color(t.muted_foreground),
                    );
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 260.0) / 2.0);
                        egui::Grid::new("shortcuts").spacing([12.0, 6.0]).show(ui, |ui| {
                            for (keys, what) in [
                                ("Ctrl+P", "Open table or command"),
                                ("Ctrl+T", "New SQL editor"),
                                ("Ctrl+W", "Close tab"),
                                ("Ctrl+Tab", "Next tab"),
                                ("F5", "Run / reload"),
                            ] {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    keycap(ui, keys)
                                });
                                ui.label(RichText::new(what).color(t.muted_foreground));
                                ui.end_row();
                            }
                        });
                    });
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 260.0) / 2.0);
                        open_palette = ui
                            .add(theme::primary_button(Some(icon::MAGNIFYING_GLASS), "Open table"))
                            .clicked();
                        new_sql = ui.add(theme::outline_button("New SQL")).clicked();
                    });
                }
            }
        });

        if let Some(id) = connect {
            self.connect_saved(&id);
        }
        if let Some(c) = edit {
            self.connections.form = Some(ConnForm {
                editing: Some(c.id.clone()),
                cfg: c,
            });
        }
        if let Some(id) = delete {
            self.delete_connection(&id);
        }
        if new_conn {
            self.connections.form = Some(ConnForm {
                editing: None,
                cfg: ConnConfig::new(Kind::Postgres),
            });
        }
        if open_palette {
            self.open_palette();
        }
        if new_sql {
            self.open_sql(String::new());
        }
    }
}

enum CardAction {
    Connect,
    Edit,
    Delete,
}

/// One line of text cut with "…" to `width`, so a long host or file path stays inside the card.
fn one_line(ui: &egui::Ui, text: &str, font: FontId, color: egui::Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width(width);
    ui.fonts_mut(|f| f.layout_job(job))
}

/// A saved connection: click to connect, the pencil (or right click) to edit it, which is the
/// way to fix one that no longer connects.
fn connection_card(ui: &mut egui::Ui, c: &ConnConfig) -> Option<CardAction> {
    let t = theme::tokens();
    let width = CARD_WIDTH.min(ui.available_width());
    let (rect, r) = ui.allocate_exact_size(egui::vec2(width, 52.0), Sense::click());
    // Over the pencil the card itself isn't hovered, but it should still look it.
    let hovered = ui.rect_contains_pointer(rect);
    let p = ui.painter();
    p.rect_filled(rect, 0.0, if hovered { t.hover } else { t.card });
    p.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, if hovered { t.ring } else { t.border }),
        egui::StrokeKind::Inside,
    );
    let (label, color) = match c.kind {
        Kind::Postgres => ("PG", t.info),
        Kind::MySql => ("MY", t.warning),
        Kind::Sqlite => ("SL", t.boolean),
    };
    let badge = egui::Rect::from_min_size(
        rect.left_center() + egui::vec2(12.0, -14.0),
        egui::vec2(28.0, 28.0),
    );
    p.rect_filled(badge, 0.0, theme::mix(color, 0.18, t.card));
    p.text(
        badge.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.0),
        color,
    );
    // The text stops short of the pencil and the arrow on the right.
    let text_width = (rect.width() - 52.0 - 64.0).max(0.0);
    let name = one_line(ui, &c.name, FontId::proportional(13.0), t.foreground, text_width);
    p.galley(rect.left_top() + egui::vec2(52.0, 10.0), name, t.foreground);
    let db = if c.database.is_empty() {
        String::new()
    } else {
        format!(" / {}", c.database)
    };
    let address = if c.kind == Kind::Sqlite {
        c.database.clone()
    } else {
        format!("{}@{}:{}{db}", c.user, c.host, c.port)
    };
    let shown = one_line(ui, &address, FontId::proportional(12.0), t.subtle, text_width);
    let cut = shown.elided;
    p.galley(rect.left_top() + egui::vec2(52.0, 28.0), shown, t.subtle);
    p.text(
        rect.right_center() - egui::vec2(14.0, 0.0),
        Align2::RIGHT_CENTER,
        icon::ARROW_RIGHT,
        FontId::new(14.0, theme::icon_family()),
        if r.hovered() { t.accent_text } else { t.subtle },
    );

    // Drawn and sensed by hand: a placed widget would move the column's cursor.
    let pencil = egui::Rect::from_center_size(
        rect.right_center() - egui::vec2(42.0, 0.0),
        egui::vec2(24.0, 24.0),
    );
    let edit = ui
        .interact(pencil, r.id.with("edit"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Edit connection");
    if edit.hovered() {
        ui.painter().rect_filled(pencil, 0.0, t.card);
    }
    ui.painter().text(
        pencil.center(),
        Align2::CENTER_CENTER,
        icon::PENCIL_SIMPLE,
        FontId::new(14.0, theme::icon_family()),
        if edit.hovered() {
            t.accent_text
        } else if hovered {
            t.foreground
        } else {
            t.subtle
        },
    );
    let edit = edit.clicked();
    let mut action = None;
    r.context_menu(|ui| {
        if ui.button("Edit").clicked() {
            action = Some(CardAction::Edit);
            ui.close();
        }
        if ui.button("Delete").clicked() {
            action = Some(CardAction::Delete);
            ui.close();
        }
    });
    // The whole address, when the card had to cut it.
    let r = if cut { r.on_hover_text(address) } else { r };
    if edit {
        Some(CardAction::Edit)
    } else if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        Some(CardAction::Connect)
    } else {
        action
    }
}
