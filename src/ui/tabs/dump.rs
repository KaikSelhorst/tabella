//! "Exportar .sql": options, progress, and the file it wrote.

use super::*;

pub(super) fn dump_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let t = theme::tokens();
    let TabKind::Dump(st) = &mut tab.kind else {
        return;
    };
    let (mut start, mut cancel) = (false, false);
    egui::ScrollArea::vertical()
        .id_salt(("dump", tab.id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .fill(t.background)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::icon(icon::DOWNLOAD_SIMPLE).size(15.0).color(t.muted_foreground));
                        ui.label(egui::RichText::new("Exportar como .sql").size(15.0).color(t.foreground));
                    });
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(&st.what).monospace().color(t.foreground));
                    let mut detail = format!("{} tabela(s)", st.tables.len());
                    if !st.objects_of.is_empty() {
                        detail.push_str(" · views, funções, procedures e triggers");
                    }
                    detail.push_str(&format!(" · de {}", s.cfg.name));
                    ui.label(egui::RichText::new(detail).size(12.0).color(t.muted_foreground));
                    ui.add_space(8.0);
                    ui.add_enabled_ui(!st.running, |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut st.structure, "estrutura");
                            ui.checkbox(&mut st.data, "dados");
                            ui.add_enabled(st.structure, egui::Checkbox::new(&mut st.drop, "DROP antes de criar"))
                                .on_hover_text("Para substituir o que já existir no destino com o mesmo nome");
                        });
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().interact_size.y = 28.0;
                        if st.running {
                            if ui.add(theme::outline_button("Cancelar")).clicked() {
                                cancel = true;
                            }
                        } else {
                            let label = if st.outcome.is_some() { "Exportar de novo" } else { "Exportar" };
                            if ui
                                .add_enabled(
                                    st.structure || st.data,
                                    theme::primary_button(Some(icon::DOWNLOAD_SIMPLE), label),
                                )
                                .clicked()
                            {
                                start = true;
                            }
                        }
                    });
                    let hint = match s.cfg.kind {
                        crate::db::Kind::Postgres => {
                            "Salvo em Downloads. Tabelas sem chaves estrangeiras, os dados, depois as chaves e as \
                             sequences: roda com \"Executar arquivo .sql\" ou psql."
                        }
                        crate::db::Kind::MySql => {
                            "Salvo em Downloads, sem o nome do banco: execute no banco de destino (\"Executar \
                             arquivo .sql\" ou mysql)."
                        }
                    };
                    ui.label(egui::RichText::new(hint).size(11.0).color(t.subtle));
                    if !st.running && st.outcome.is_none() {
                        return;
                    }
                    ui.add_space(12.0);
                    let p = st.progress;
                    let frac = if p.total == 0 { 1.0 } else { p.tables as f32 / p.total as f32 };
                    ui.horizontal(|ui| {
                        let fill = match &st.outcome {
                            Some(Err(_)) => t.destructive,
                            _ => t.lime,
                        };
                        ui.add(egui::ProgressBar::new(frac).desired_width(320.0).desired_height(6.0).fill(fill));
                        let took = st.took.or_else(|| st.started.map(|at| at.elapsed())).unwrap_or_default();
                        let text = format!(
                            "{} de {} tabelas · {} linhas · {} · {:.1}s",
                            p.tables,
                            p.total,
                            palette::format_count(p.rows as i64),
                            palette::format_bytes(p.bytes as i64),
                            took.as_secs_f32()
                        );
                        ui.label(egui::RichText::new(text).size(12.0).color(t.muted_foreground));
                    });
                    if st.running {
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                        return;
                    }
                    ui.add_space(8.0);
                    match &st.outcome {
                        Some(Ok(path)) => {
                            ui.horizontal(|ui| {
                                ui.label(theme::icon(icon::CHECK).size(16.0).color(t.lime));
                                ui.label(egui::RichText::new(path.display().to_string()).monospace().color(t.foreground));
                            });
                            ui.add_space(4.0);
                            if ui.add(theme::outline_button("Mostrar na pasta")).clicked() {
                                export::reveal(path);
                            }
                        }
                        Some(Err(e)) => {
                            let title = if e == "cancelado" {
                                "Cancelado: o arquivo incompleto foi apagado."
                            } else {
                                "A exportação falhou; o arquivo incompleto foi apagado."
                            };
                            ui.horizontal(|ui| {
                                ui.label(theme::icon(icon::WARNING).size(16.0).color(t.destructive));
                                ui.label(egui::RichText::new(title).color(t.foreground));
                            });
                            if e != "cancelado" {
                                ui.add_space(4.0);
                                error_banner(ui, e);
                            }
                        }
                        None => {}
                    }
                });
        });
    if start {
        app::start_dump(tab, s);
    } else if cancel {
        app::cancel_dump(tab, s);
    }
}
