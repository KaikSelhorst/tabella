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
        crate::db::set_max_rows(s.row_limit);
        if let Err(e) = settings::save(&s) {
            self.notice = crate::app::Notice::error(format!("Error saving preferences: {e}"));
        }
        self.settings = s;
    }

    /// Picking a theme by hand stops following the system.
    pub fn toggle_theme(&mut self, ctx: &egui::Context) {
        let s = Settings {
            light: !self.settings.light,
            system_theme: false,
            ..self.settings.clone()
        };
        self.apply_settings(ctx, s);
    }

    /// Next of the row limits (back to the first after the last).
    pub fn cycle_row_limit(&mut self, ctx: &egui::Context) {
        let next = settings::ROW_LIMITS
            .iter()
            .copied()
            .find(|n| *n > self.settings.row_limit)
            .unwrap_or(settings::ROW_LIMITS[0]);
        let s = Settings {
            row_limit: next,
            ..self.settings.clone()
        };
        self.apply_settings(ctx, s);
    }

    pub fn toggle_system_theme(&mut self, ctx: &egui::Context) {
        let s = Settings {
            system_theme: !self.settings.system_theme,
            ..self.settings.clone()
        };
        self.apply_settings(ctx, s);
    }

    /// With "follow the system" on, switches to the operating system's theme whenever it changes
    /// (and at start, once the window can say). Where the system doesn't tell, nothing changes.
    pub fn follow_system_theme(&mut self, ctx: &egui::Context) {
        if !self.settings.system_theme {
            return;
        }
        let Some(system) = ctx.system_theme() else { return };
        let light = system == egui::Theme::Light;
        if light != self.settings.light {
            let s = Settings {
                light,
                ..self.settings.clone()
            };
            self.apply_settings(ctx, s);
        }
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

        // "Follow the system" tracks the OS theme as it changes; choosing by hand stops it.
        harness.state_mut().settings.system_theme = true;
        harness.input_mut().system_theme = Some(egui::Theme::Light);
        harness.run();
        assert!(super::theme::is_light() && harness.state().settings.light);
        harness.input_mut().system_theme = Some(egui::Theme::Dark);
        harness.run();
        assert!(!super::theme::is_light() && !harness.state().settings.light);
        // Choosing by hand wins over the system until it is turned on again.
        harness.state_mut().toggle_theme(&ctx);
        assert!(!harness.state().settings.system_theme && super::theme::is_light());
        harness.run();
        assert!(
            super::theme::is_light(),
            "the system's dark theme no longer applies"
        );
        harness.state_mut().toggle_system_theme(&ctx);
        harness.run();
        assert!(
            !super::theme::is_light(),
            "following again picks the system's theme up"
        );
        harness.state_mut().toggle_system_theme(&ctx);
    }
}
