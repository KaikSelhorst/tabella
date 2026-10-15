//! Theme and zoom: switched from Ctrl+P (or the zoom keys), applied at once and remembered.

use eframe::egui;

use super::theme;
use crate::app::App;
use crate::settings::{self, Settings, ZOOM, ZOOM_STEP};

impl App {
    /// Makes `s` current (theme, zoom) and saves it.
    pub fn apply_settings(&mut self, ctx: &egui::Context, s: Settings) {
        let s = s.clamped();
        if s.light != self.settings.light {
            theme::set_light(s.light);
            theme::apply(ctx);
        }
        if (s.zoom - ctx.zoom_factor()).abs() > 0.001 {
            ctx.set_zoom_factor(s.zoom);
        }
        if let Err(e) = settings::save(&s) {
            self.notice = crate::app::Notice::error(format!("Error saving preferences: {e}"));
        }
        self.settings = s;
    }

    pub fn toggle_theme(&mut self, ctx: &egui::Context) {
        let s = Settings {
            light: !self.settings.light,
            ..self.settings.clone()
        };
        self.apply_settings(ctx, s);
    }

    /// `+1` / `-1` steps of 10%; `0` back to 100%.
    pub fn zoom_by(&mut self, ctx: &egui::Context, steps: i32) {
        let zoom = if steps == 0 {
            1.0
        } else {
            // Rounded to the step so repeated changes land on 110%, 120%… not 109.99%.
            ((ctx.zoom_factor() + steps as f32 * ZOOM_STEP) / ZOOM_STEP).round() * ZOOM_STEP
        };
        let s = Settings {
            zoom: zoom.clamp(*ZOOM.start(), *ZOOM.end()),
            ..self.settings.clone()
        };
        self.apply_settings(ctx, s);
    }

    /// Ctrl + / Ctrl − change the zoom by themselves; remember what they set.
    pub fn track_zoom(&mut self, ctx: &egui::Context) {
        let z = ctx.zoom_factor();
        if (z - self.settings.zoom).abs() > 0.001 {
            let s = Settings {
                zoom: z,
                ..self.settings.clone()
            };
            self.apply_settings(ctx, s);
        }
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui;
    use egui_kittest::Harness;

    /// Theme and zoom from the palette commands: applied at once and remembered.
    #[test]
    fn theme_and_zoom_switch_and_stick() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1280.0, 760.0))
            .build_eframe(|cc| {
                super::theme::apply(&cc.egui_ctx);
                super::super::screenshots::fake_app(&cc.egui_ctx)
            });
        harness.run();
        let ctx = harness.ctx.clone();
        harness.state_mut().zoom_by(&ctx, 2);
        harness.run();
        assert!((harness.ctx.zoom_factor() - 1.2).abs() < 1e-4);
        assert!((harness.state().settings.zoom - 1.2).abs() < 1e-4);
        harness.state_mut().zoom_by(&ctx, 0);
        harness.run();
        assert_eq!(harness.ctx.zoom_factor(), 1.0);
        harness.state_mut().toggle_theme(&ctx);
        assert!(super::theme::is_light() && harness.state().settings.light);
        harness.state_mut().toggle_theme(&ctx);
        assert!(!super::theme::is_light());
    }
}
