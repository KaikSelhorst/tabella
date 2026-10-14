//! "Importar CSV" dialog: pick the file, check the preview, map file columns to table columns.

use eframe::egui::{self, Align, Layout, RichText, TextEdit};

use super::{icon, theme};
use crate::app::ImportForm;
use crate::import::PREVIEW_ROWS;

pub enum ImportAction {
    Submit,
    Cancel,
}

/// Width of a preview column.
const COL_WIDTH: f32 = 150.0;

pub fn import_form(
    ctx: &egui::Context,
    tab_id: u64,
    table: &str,
    columns: &[String],
    form: &mut ImportForm,
    busy: bool,
) -> Option<ImportAction> {
    let t = theme::tokens();
    let mut action = None;
    let modal = egui::Modal::new(egui::Id::new(("import_form", tab_id)))
        .frame(theme::modal_frame(16))
        .show(ctx, |ui| {
            ui.set_width(720.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::UPLOAD_SIMPLE).size(16.0).color(t.lime));
                ui.label(RichText::new(format!("Importar CSV em {table}")).size(15.0).color(t.foreground));
            });
            ui.label(
                RichText::new("Tudo numa transação: se uma linha falhar, nada é importado. Números como 1.520,30 e booleanos como sim/não são convertidos.")
                    .size(11.0)
                    .color(t.subtle),
            );
            ui.add_space(8.0);
            ui.add(
                TextEdit::singleline(&mut form.path)
                    .hint_text("Caminho do arquivo .csv (ou arraste o arquivo para a janela)")
                    .font(egui::FontId::monospace(12.5))
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut form.has_header, "Primeira linha é o cabeçalho");
                ui.checkbox(&mut form.empty_is_null, "Célula vazia vira NULL");
            });
            ui.add_space(8.0);

            match &form.preview {
                None => {
                    ui.label(RichText::new("Escolha um arquivo para ver a prévia.").color(t.subtle));
                }
                Some((_, _, Err(e))) => {
                    ui.label(RichText::new(e).color(t.destructive));
                }
                Some((_, _, Ok(pv))) => {
                    let sep = match pv.delimiter {
                        b';' => "ponto e vírgula",
                        b'\t' => "tab",
                        _ => "vírgula",
                    };
                    ui.label(
                        RichText::new(format!(
                            "{} coluna(s) · separador: {sep} · prévia das primeiras {PREVIEW_ROWS} linhas",
                            pv.headers.len()
                        ))
                        .size(11.0)
                        .color(t.subtle),
                    );
                    ui.add_space(4.0);
                    egui::ScrollArea::horizontal()
                        .id_salt(("import_preview", tab_id))
                        .show(ui, |ui| {
                            egui::Grid::new(("import_grid", tab_id))
                                .spacing([8.0, 4.0])
                                .min_col_width(COL_WIDTH)
                                .max_col_width(COL_WIDTH)
                                .show(ui, |ui| {
                                    for h in &pv.headers {
                                        ui.add(
                                            egui::Label::new(RichText::new(h).color(t.foreground)).truncate(),
                                        );
                                    }
                                    ui.end_row();
                                    // Where each file column goes.
                                    for (i, target) in form.mapping.iter_mut().enumerate() {
                                        let shown = target.as_deref().unwrap_or("(ignorar)");
                                        let color = if target.is_some() { t.lime } else { t.subtle };
                                        egui::ComboBox::from_id_salt(("import_map", tab_id, i))
                                            .width(COL_WIDTH)
                                            .selected_text(RichText::new(shown).color(color))
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(target, None, "(ignorar)");
                                                for c in columns {
                                                    ui.selectable_value(target, Some(c.clone()), c);
                                                }
                                            });
                                    }
                                    ui.end_row();
                                    for row in &pv.rows {
                                        for i in 0..pv.headers.len() {
                                            let v = row.get(i).map_or("", String::as_str);
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(v).monospace().size(12.0).color(t.muted_foreground),
                                                )
                                                .truncate(),
                                            );
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                }
            }

            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ready = !busy
                    && matches!(&form.preview, Some((_, _, Ok(_))))
                    && form.mapping.iter().any(Option::is_some);
                if ui
                    .add_enabled(ready, theme::primary_button(Some(icon::UPLOAD_SIMPLE), "Importar"))
                    .clicked()
                {
                    action = Some(ImportAction::Submit);
                }
                if ui.add(theme::outline_button("Cancelar")).clicked() {
                    action = Some(ImportAction::Cancel);
                }
                if busy {
                    ui.spinner();
                    ui.label(RichText::new("Importando…").color(t.muted_foreground));
                }
            });
        });
    if modal.should_close() && action.is_none() && !busy {
        action = Some(ImportAction::Cancel);
    }
    action
}
