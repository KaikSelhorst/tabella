//! The full query history tab.

use super::*;

/// Every query run, newest first: search, filter, look at one, open it in an editor.
pub(super) fn history_tab(ui: &mut egui::Ui, tab: &mut Tab, cx: &mut Cx) {
    let t = theme::tokens();
    let conn = cx.s.cfg.name.clone();
    let TabKind::History(st) = &mut tab.kind else {
        return;
    };
    egui::Frame::new()
        .fill(t.background)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    theme::icon(icon::CLOCK_COUNTER_CLOCKWISE)
                        .size(15.0)
                        .color(t.muted_foreground),
                );
                ui.label(
                    egui::RichText::new("Histórico de consultas")
                        .size(15.0)
                        .color(t.foreground),
                );
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let r = ui.add(
                    TextEdit::singleline(&mut st.query)
                        .hint_text("Buscar no texto das consultas")
                        .desired_width(320.0),
                );
                if std::mem::take(&mut st.focus) {
                    r.request_focus();
                }
                ui.checkbox(&mut st.all_conns, "todas as conexões");
                ui.checkbox(&mut st.only_errors, "só as que falharam");
            });
        });
    let needle = st.query.to_lowercase();
    let list: Vec<&Entry> = cx
        .history
        .iter()
        .rev()
        .filter(|e| st.all_conns || e.conn == conn)
        .filter(|e| !st.only_errors || !e.ok)
        .filter(|e| needle.is_empty() || e.sql.to_lowercase().contains(&needle))
        .collect();
    let picked = st
        .selected
        .as_ref()
        .and_then(|(c, q)| list.iter().find(|e| e.conn == *c && e.sql == *q).copied());

    // The chosen one, whole, with what can be done with it.
    if let Some(e) = picked {
        egui::Panel::bottom(egui::Id::new(("history_detail", tab.id)))
            .resizable(true)
            .default_size(200.0)
            .frame(
                egui::Frame::new()
                    .fill(t.sidebar)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let when = history::ago(e.at, history::now());
                    let status = if e.ok {
                        format!("{} ms", e.ms)
                    } else {
                        "falhou".to_string()
                    };
                    ui.label(
                        egui::RichText::new(format!("{} · {when} · {status}", e.conn))
                            .size(12.0)
                            .color(t.subtle),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(theme::primary_button(Some(icon::CODE), "Abrir no editor"))
                            .clicked()
                        {
                            cx.request = Some(Request::Sql(e.sql.clone()));
                        }
                        if ui
                            .button(theme::icon_text(
                                icon::COPY,
                                "Copiar",
                                t.muted_foreground,
                                t.foreground,
                            ))
                            .clicked()
                        {
                            ui.ctx().copy_text(e.sql.clone());
                        }
                        if ui
                            .button(theme::icon_text(
                                icon::TRASH,
                                "Esquecer",
                                t.muted_foreground,
                                t.foreground,
                            ))
                            .on_hover_text("Tira esta consulta do histórico")
                            .clicked()
                        {
                            cx.request = Some(Request::ForgetHistory(e.conn.clone(), e.sql.clone()));
                        }
                    });
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let job = syntax::highlight(&e.sql, egui::FontId::monospace(12.5));
                        ui.add(egui::Label::new(job).selectable(true));
                    });
            });
    }

    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(format!("{} consulta(s)", list.len()))
                .size(11.0)
                .color(t.subtle),
        );
    });
    // Only the rows on screen are laid out: the history can hold thousands.
    let row_h = 26.0;
    let now = history::now();
    let mut select: Option<(String, String)> = None;
    let mut open: Option<String> = None;
    egui::ScrollArea::vertical()
        .id_salt(("history_list", tab.id))
        .auto_shrink([false, false])
        .show_rows(ui, row_h, list.len(), |ui, range| {
            for e in &list[range] {
                let selected = picked.is_some_and(|p| std::ptr::eq(p, *e));
                let first = e.sql.split_whitespace().collect::<Vec<_>>().join(" ");
                let color = if e.ok { t.foreground } else { t.destructive };
                let mut right = history::ago(e.at, now);
                if st.all_conns {
                    right = format!("{} · {right}", e.conn);
                }
                let r = ui.add(
                    egui::Button::selectable(
                        selected,
                        egui::RichText::new(first).monospace().size(12.0).color(color),
                    )
                    .truncate()
                    .min_size(egui::vec2(ui.available_width(), row_h - 2.0))
                    .right_text(egui::RichText::new(right).size(11.0).color(t.subtle)),
                );
                if r.clicked() {
                    select = Some((e.conn.clone(), e.sql.clone()));
                }
                if r.double_clicked() || r.triple_clicked() {
                    open = Some(e.sql.clone());
                }
            }
        });
    if select.is_some() {
        st.selected = select;
    }
    if let Some(sql) = open {
        cx.request = Some(Request::Sql(sql));
    }
}
