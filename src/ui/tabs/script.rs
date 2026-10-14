//! "Executar arquivo .sql": a file run statement by statement, with progress.

use super::*;

/// A `.sql` file run statement by statement: progress while it goes, then where it stopped.
pub(super) fn script_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let s = cx.s;
    let t = theme::tokens();
    let TabKind::Script(st) = &mut tab.kind else {
        return;
    };
    let (mut start, mut cancel) = (false, false);
    egui::ScrollArea::vertical()
        .id_salt(("script", tab.id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .fill(t.background)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::icon(icon::PLAY).size(15.0).color(t.muted_foreground));
                        ui.label(egui::RichText::new("Executar arquivo .sql").size(15.0).color(t.foreground));
                    });
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(st.path.display().to_string())
                            .monospace()
                            .color(t.foreground),
                    );
                    let here = match s.cfg.kind {
                        crate::db::Kind::Postgres => format!("{} · {}", s.current_db, s.current_schema),
                        crate::db::Kind::MySql => s.current_db.clone(),
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · em {} ({})",
                            super::palette::format_bytes(st.size as i64),
                            here,
                            s.cfg.name
                        ))
                        .size(12.0)
                        .color(t.muted_foreground),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().interact_size.y = 28.0;
                        if st.running {
                            if ui.add(theme::outline_button("Cancelar")).clicked() {
                                cancel = true;
                            }
                        } else {
                            let label = if st.outcome.is_some() { "Executar de novo" } else { "Executar" };
                            if ui.add(theme::primary_button(Some(icon::PLAY), label)).clicked() {
                                start = true;
                            }
                        }
                        let hint = match s.cfg.kind {
                            crate::db::Kind::Postgres => {
                                "Tudo ou nada: no primeiro erro (ou ao cancelar) nada do arquivo fica no banco"
                            }
                            crate::db::Kind::MySql => {
                                "No MySQL, cada CREATE/ALTER/DROP confirma o que veio antes: num erro, só volta \
                                 atrás o que rodou depois do último deles"
                            }
                        };
                        ui.add_enabled(!st.running, egui::Checkbox::new(&mut st.transaction, "numa transação"))
                            .on_hover_text(hint);
                    });
                    ui.label(
                        egui::RichText::new(
                            "Lê o arquivo aos poucos e para no primeiro erro. Entende DELIMITER (MySQL), blocos \
                             $$ e COPY … FROM stdin do pg_dump; comandos do psql (\\connect…) são pulados.",
                        )
                        .size(11.0)
                        .color(t.subtle),
                    );
                    if !st.running && st.outcome.is_none() {
                        return;
                    }
                    ui.add_space(12.0);
                    let p = st.progress;
                    let frac = if st.size == 0 { 1.0 } else { (p.bytes as f32 / st.size as f32).min(1.0) };
                    ui.horizontal(|ui| {
                        let fill = match &st.outcome {
                            Some(Some(_)) => t.destructive,
                            _ => t.lime,
                        };
                        ui.add(egui::ProgressBar::new(frac).desired_width(320.0).desired_height(6.0).fill(fill));
                        let took = st.took.or_else(|| st.started.map(|at| at.elapsed())).unwrap_or_default();
                        let plural = |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
                        let mut text = format!(
                            "{:.0}% · {} · linha {} · {:.1}s",
                            frac * 100.0,
                            plural(p.statements, "comando", "comandos"),
                            p.line,
                            took.as_secs_f32()
                        );
                        if p.copied > 0 {
                            text.push_str(&format!(" · {} via COPY", plural(p.copied, "linha", "linhas")));
                        }
                        if p.skipped > 0 {
                            let skipped = plural(p.skipped, "comando do psql pulado", "comandos do psql pulados");
                            text.push_str(&format!(" · {skipped}"));
                        }
                        ui.label(egui::RichText::new(text).size(12.0).color(t.muted_foreground));
                    });
                    if st.running {
                        // The worker reports a few times a second; keep the clock moving.
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                        return;
                    }
                    ui.add_space(8.0);
                    match &st.outcome {
                        Some(None) => {
                            ui.horizontal(|ui| {
                                ui.label(theme::icon(icon::CHECK).size(16.0).color(t.lime));
                                let done = if st.transaction { "Arquivo executado e confirmado" } else { "Arquivo executado" };
                                ui.label(egui::RichText::new(done).color(t.foreground));
                            });
                        }
                        Some(Some(e)) => {
                            let undone = if st.transaction {
                                " Nada do arquivo ficou no banco (transação desfeita)."
                            } else {
                                " O que rodou antes disso continua no banco."
                            };
                            let title = if e.cancelled {
                                format!("Cancelado na linha {}.{undone}", e.line)
                            } else {
                                format!("Erro na linha {}.{undone}", e.line)
                            };
                            ui.horizontal(|ui| {
                                ui.label(theme::icon(icon::WARNING).size(16.0).color(t.destructive));
                                ui.label(egui::RichText::new(title).color(t.foreground));
                            });
                            if !e.cancelled {
                                ui.add_space(4.0);
                                error_banner(ui, &e.message);
                            }
                            if !e.statement.is_empty() {
                                ui.add_space(4.0);
                                egui::Frame::new()
                                    .fill(t.card)
                                    .stroke(egui::Stroke::new(1.0, t.border))
                                    .inner_margin(egui::Margin::same(8))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(
                                            egui::RichText::new(&e.statement)
                                                .monospace()
                                                .size(12.0)
                                                .color(t.foreground),
                                        );
                                    });
                            }
                        }
                        None => {}
                    }
                });
        });
    if start {
        app::start_script(tab, s);
    } else if cancel {
        app::cancel_script(tab, s);
    }
}
