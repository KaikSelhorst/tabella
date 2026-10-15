//! Empty states for the main area: connection cards before connecting, shortcuts once connected.

use eframe::egui::{self, Align2, FontId, RichText, Sense};

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
                    .size(11.0)
                    .color(t.muted_foreground),
            )
        });
}

impl App {
    pub fn welcome(&mut self, ui: &mut egui::Ui) {
        let t = theme::tokens();
        let mut connect = None;
        let mut new_conn = false;
        let mut open_palette = false;
        let mut new_sql = false;

        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.16).max(24.0));
            ui.label(theme::icon(icon::DATABASE).size(36.0).color(t.lime));
            ui.add_space(4.0);
            ui.label(RichText::new("tabella").size(22.0).color(t.foreground));
            ui.label(RichText::new("Lightweight client for PostgreSQL, MySQL and SQLite").color(t.muted_foreground));
            ui.add_space(24.0);

            match &self.session {
                None => {
                    for c in &self.saved {
                        if connection_card(ui, c).clicked() {
                            connect = Some(c.id.clone());
                        }
                        ui.add_space(6.0);
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
                            .add(theme::primary_button(
                                Some(icon::MAGNIFYING_GLASS),
                                "Open table",
                            ))
                            .clicked();
                        new_sql = ui.add(theme::outline_button("New SQL")).clicked();
                    });
                }
            }
        });

        if let Some(id) = connect {
            self.connect_saved(&id);
        }
        if new_conn {
            self.form = Some(ConnForm {
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

fn connection_card(ui: &mut egui::Ui, c: &ConnConfig) -> egui::Response {
    let t = theme::tokens();
    let (rect, r) = ui.allocate_exact_size(egui::vec2(CARD_WIDTH, 52.0), Sense::click());
    let hovered = r.hovered();
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
        FontId::proportional(11.0),
        color,
    );
    p.text(
        rect.left_top() + egui::vec2(52.0, 10.0),
        Align2::LEFT_TOP,
        &c.name,
        FontId::proportional(13.0),
        t.foreground,
    );
    let db = if c.database.is_empty() {
        String::new()
    } else {
        format!(" / {}", c.database)
    };
    p.text(
        rect.left_top() + egui::vec2(52.0, 28.0),
        Align2::LEFT_TOP,
        if c.kind == Kind::Sqlite {
            c.database.clone()
        } else {
            format!("{}@{}:{}{db}", c.user, c.host, c.port)
        },
        FontId::proportional(11.0),
        t.subtle,
    );
    p.text(
        rect.right_center() - egui::vec2(14.0, 0.0),
        Align2::RIGHT_CENTER,
        icon::ARROW_RIGHT,
        FontId::new(14.0, theme::icon_family()),
        if hovered { t.lime } else { t.subtle },
    );
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}
