//! "Buscar valor em todas as tabelas".

use super::*;
use std::fmt::Write as _;

/// "Buscar valor em todas as tabelas": the term, options, progress and where it was found.
pub(super) fn search_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let t = theme::tokens();
    let TabKind::Search(st) = &mut tab.kind else {
        return;
    };
    let (mut start, mut cancel) = (false, false);
    let mut open: Option<(String, String, String)> = None;
    egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::MAGNIFYING_GLASS).size(15.0).color(t.muted_foreground));
                ui.label(egui::RichText::new("Buscar valor em todas as tabelas").size(15.0).color(t.foreground));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().interact_size.y = 28.0;
                let r = ui.add_enabled(
                    !st.running,
                    TextEdit::singleline(&mut st.term)
                        .hint_text("e-mail, CPF, código, id…  (Enter busca)")
                        .font(egui::FontId::monospace(13.0))
                        .margin(egui::Margin::symmetric(8, 6))
                        .desired_width(360.0),
                );
                if std::mem::take(&mut st.focus) {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    start = true;
                }
                if st.running {
                    if ui.add(theme::outline_button("Cancelar")).clicked() {
                        cancel = true;
                    }
                } else if ui
                    .add_enabled(!st.term.trim().is_empty(), theme::primary_button(Some(icon::MAGNIFYING_GLASS), "Buscar"))
                    .clicked()
                {
                    start = true;
                }
                ui.add_enabled(!st.running, egui::Checkbox::new(&mut st.exact, "valor exato"))
                    .on_hover_text("Desmarcado: procura o texto dentro dos valores (qualquer caixa)");
                ui.add_enabled(
                    !st.running,
                    egui::Checkbox::new(&mut st.include_big, "incluir tabelas grandes"),
                )
                .on_hover_text("Tabelas com mais de 1 milhão de linhas: cada uma é lida inteira e leva segundos");
            });
            ui.label(
                egui::RichText::new(
                    "Lê as tabelas uma a uma (só colunas onde o valor caberia). Enquanto isso, as outras abas \
                     desta conexão esperam.",
                )
                .size(11.0)
                .color(t.subtle),
            );
            if st.running || st.total > 0 {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let frac = if st.total == 0 { 0.0 } else { st.done as f32 / st.total as f32 };
                    ui.add(egui::ProgressBar::new(frac).desired_width(240.0).desired_height(6.0).fill(t.lime));
                    let mut text = format!(
                        "{} de {} tabelas · encontrado em {}",
                        st.done,
                        st.total,
                        st.hits.len()
                    );
                    if st.skipped > 0 {
                        let _ = write!(text, " · {} grandes puladas", st.skipped);
                    }
                    if !st.errors.is_empty() {
                        let _ = write!(text, " · {} com erro", st.errors.len());
                    }
                    let r = ui.label(egui::RichText::new(text).size(12.0).color(t.muted_foreground));
                    if !st.errors.is_empty() {
                        r.on_hover_text(st.errors.join("\n"));
                    }
                });
            }
        });

    egui::ScrollArea::vertical()
        .id_salt(("search_hits", tab.id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 4))
                .show(ui, |ui| {
                    if st.hits.is_empty() {
                        if !st.running && st.total > 0 {
                            ui.label(egui::RichText::new("Nada encontrado").color(t.subtle));
                        }
                        return;
                    }
                    egui::Grid::new(("search_grid", tab.id))
                        .num_columns(4)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            for h in ["tabela", "colunas", "linhas", ""] {
                                ui.label(egui::RichText::new(h).size(11.0).color(t.subtle));
                            }
                            ui.end_row();
                            for h in &st.hits {
                                ui.label(
                                    egui::RichText::new(format!("{}.{}", h.schema, h.table))
                                        .monospace()
                                        .color(t.foreground),
                                );
                                ui.label(
                                    egui::RichText::new(h.columns.join(", "))
                                        .monospace()
                                        .color(t.info),
                                );
                                let rows = if h.rows >= crate::search::ROWS_PER_TABLE {
                                    format!("{}+", h.rows)
                                } else {
                                    h.rows.to_string()
                                };
                                ui.label(egui::RichText::new(rows).monospace().color(t.number));
                                if ui
                                    .button(theme::icon_text(
                                        icon::TABLE,
                                        "Abrir",
                                        t.muted_foreground,
                                        t.foreground,
                                    ))
                                    .on_hover_text(egui::RichText::new(&h.filter).monospace())
                                    .clicked()
                                {
                                    open = Some((h.schema.clone(), h.table.clone(), h.filter.clone()));
                                }
                                ui.end_row();
                            }
                        });
                });
        });
    if start {
        app::start_search(tab, s);
    } else if cancel {
        app::cancel_search(tab, s);
    }
    if let Some((schema, table, filter)) = open {
        cx.request = Some(Request::OpenFiltered(schema, table, filter));
    }
}
