//! The app's notices as a toast in the bottom-right corner: information fades out by itself,
//! errors stay until closed (they explain something that didn't happen).

use std::time::Duration;

use eframe::egui;

use super::{icon, theme};
use crate::app::App;

/// How long an information notice stays.
const INFO_FOR: Duration = Duration::from_secs(5);

impl App {
    pub fn notice_toast(&mut self, ctx: &egui::Context) {
        let Some(n) = &self.notice else { return };
        if !n.error {
            let shown = n.at.elapsed();
            if shown >= INFO_FOR {
                self.notice = None;
                return;
            }
            ctx.request_repaint_after(INFO_FOR - shown);
        }
        let t = theme::tokens();
        let (glyph, accent) = if n.error {
            (icon::WARNING_CIRCLE, t.destructive)
        } else {
            (icon::CHECK_CIRCLE, t.lime)
        };
        let mut close = false;
        egui::Area::new(egui::Id::new("notice_toast"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -36.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(t.card)
                    .stroke(egui::Stroke::new(1.0, theme::mix(accent, 0.6, t.border)))
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_max_width(440.0);
                        ui.horizontal(|ui| {
                            ui.label(theme::icon(glyph).size(16.0).color(accent));
                            let color = if n.error { t.foreground } else { t.muted_foreground };
                            ui.add(egui::Label::new(egui::RichText::new(&n.text).color(color)).wrap());
                            if n.error {
                                let copy =
                                    egui::Button::new(theme::icon(icon::COPY).color(t.muted_foreground))
                                        .frame(false);
                                if ui.add(copy).on_hover_text("Copy the message").clicked() {
                                    ui.ctx().copy_text(n.text.clone());
                                }
                            }
                            let x = egui::Button::new(theme::icon(icon::X).color(t.muted_foreground))
                                .frame(false);
                            close = ui.add(x).on_hover_text("Close").clicked();
                        });
                    });
            });
        if close {
            self.notice = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use eframe::egui;

    use crate::app::Notice;

    #[test]
    fn information_fades_errors_stay() {
        let ctx = egui::Context::default();
        super::super::theme::apply(&ctx);
        let mut app = super::super::screenshots::fake_app(&ctx);
        let old = Instant::now() - super::INFO_FOR - std::time::Duration::from_millis(1);
        let frame = |app: &mut crate::app::App| {
            // No renderer here: the font textures are let go.
            ctx.run_ui(Default::default(), |ui| app.notice_toast(ui.ctx()))
                .textures_delta
                .clear();
        };
        app.notice = Notice::info("done");
        frame(&mut app);
        assert!(app.notice.is_some(), "a fresh one shows");
        app.notice.as_mut().unwrap().at = old;
        frame(&mut app);
        assert!(app.notice.is_none(), "information goes away by itself");
        app.notice = Notice::error("failed");
        app.notice.as_mut().unwrap().at = old;
        frame(&mut app);
        assert!(app.notice.is_some(), "an error stays until closed");
    }
}
