//! "Preferências": theme, zoom, font sizes, paging and number format, applied as they change.

use eframe::egui::{self, Align, Layout, RichText};

use super::{icon, theme};
use crate::app::App;
use crate::settings::{self, FONT, PAGE, ROWS, Settings, ZOOM};

impl App {
    pub fn prefs_ui(&mut self, ctx: &egui::Context) {
        if !self.prefs_open {
            return;
        }
        let t = theme::tokens();
        let mut s = self.settings.clone();
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("prefs"))
            .frame(theme::modal_frame(16))
            .show(ctx, |ui| {
                ui.set_width(460.0);
                // The slider rail takes the controls' fill, which is the dialog's own color:
                // a step darker here so it shows, with the part up to the value in the accent.
                ui.spacing_mut().slider_width = 180.0;
                ui.visuals_mut().widgets.inactive.bg_fill = t.border;
                ui.visuals_mut().selection.bg_fill = theme::mix(t.lime, 0.55, t.card);
                ui.horizontal(|ui| {
                    ui.label(theme::icon(icon::GEAR).size(16.0).color(t.lime));
                    ui.label(RichText::new("Preferências").size(15.0).color(t.foreground));
                });
                ui.add_space(12.0);
                egui::Grid::new("prefs_grid")
                    .num_columns(2)
                    .spacing([16.0, 10.0])
                    .show(ui, |ui| {
                        ui.label("Tema");
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            for (light, label) in [(false, "Escuro"), (true, "Claro")] {
                                if ui
                                    .add(egui::Button::new(label).selected(s.light == light))
                                    .clicked()
                                {
                                    s.light = light;
                                }
                            }
                        });
                        ui.end_row();

                        ui.label("Zoom da interface");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Slider::new(&mut s.zoom, ZOOM)
                                    .trailing_fill(true)
                                    .step_by(0.05)
                                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                            );
                            ui.label(RichText::new("Ctrl + / Ctrl −").size(11.0).color(t.subtle));
                        });
                        ui.end_row();

                        ui.label("Fonte do editor");
                        ui.add(
                            egui::Slider::new(&mut s.editor_font, FONT)
                                .trailing_fill(true)
                                .step_by(0.5)
                                .suffix(" pt"),
                        );
                        ui.end_row();

                        ui.label("Fonte da grade");
                        ui.add(
                            egui::Slider::new(&mut s.grid_font, FONT)
                                .trailing_fill(true)
                                .step_by(0.5)
                                .suffix(" pt"),
                        );
                        ui.end_row();

                        ui.label("Linhas por página");
                        ui.add(egui::DragValue::new(&mut s.page_size).range(PAGE).speed(10))
                            .on_hover_text("Nas abas de tabela; vale a partir da próxima página carregada");
                        ui.end_row();

                        ui.label("Máximo de linhas por consulta");
                        ui.add(egui::DragValue::new(&mut s.max_rows).range(ROWS).speed(100))
                            .on_hover_text("Resultados do editor acima disso são cortados (e avisam)");
                        ui.end_row();

                        ui.label("Números");
                        ui.checkbox(&mut s.decimal_comma, "vírgula decimal (1234,5)")
                            .on_hover_text(
                                "Só na exibição: editar e copiar usam o valor como o banco guarda",
                            );
                        ui.end_row();
                    });
                ui.add_space(16.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(theme::primary_button(None, "Fechar")).clicked() {
                        close = true;
                    }
                    if ui.add(theme::outline_button("Restaurar padrão")).clicked() {
                        s = Settings::default();
                    }
                });
            });
        if s != self.settings {
            self.apply_settings(ctx, s);
        }
        if close || modal.should_close() {
            self.prefs_open = false;
        }
    }

    /// Makes `s` current: theme, zoom and the values the grid and editor read; and saves it.
    pub fn apply_settings(&mut self, ctx: &egui::Context, s: Settings) {
        let s = s.clamped();
        if s.light != self.settings.light {
            theme::set_light(s.light);
            theme::apply(ctx);
        }
        if (s.zoom - ctx.zoom_factor()).abs() > 0.001 {
            ctx.set_zoom_factor(s.zoom);
        }
        settings::publish(&s);
        if let Err(e) = settings::save(&s) {
            self.notice = crate::app::Notice::error(format!("Erro ao salvar preferências: {e}"));
        }
        self.settings = s;
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
