//! Ctrl+P command palette: one search box for tables, connections and actions.

use eframe::egui::{self, Key, TextEdit};

use super::{icon, theme};
use crate::app::{App, ConnForm, Palette};
use crate::db::{ConnConfig, Kind};
use crate::favorites;
use crate::history;

const MAX_RESULTS: usize = 12;

#[derive(Clone)]
enum Cmd {
    OpenTable(String, String),
    /// Saved connection id.
    Connect(String),
    NewSql,
    OpenSql(String),
    NewConnection,
    Disconnect,
}

#[derive(Clone)]
struct Item {
    icon: &'static str,
    label: String,
    detail: String,
    cmd: Cmd,
}

/// Subsequence match, case-insensitive. Higher is better; `None` if `query` doesn't match.
/// Rewards matches at the start and runs of consecutive characters.
fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0;
    let mut pos = 0;
    let mut prev: Option<usize> = None;
    for q in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = text[pos..].iter().position(|&c| c == q)? + pos;
        score += match prev {
            Some(p) if found == p + 1 => 8,
            _ if found == 0 => 10,
            _ if found > 0 && matches!(text[found - 1], '_' | '.' | ' ' | '-') => 6,
            _ => 1,
        };
        score -= (found - pos) as i32 / 4;
        prev = Some(found);
        pos = found + 1;
    }
    // Shorter names win ties: "users" beats "users_archive" for "users".
    Some(score * 4 - text.len() as i32 / 4)
}

pub fn format_count(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(c);
    }
    out
}

/// Short form for tight spots: 950, 1,2 mil, 3,4 mi.
pub fn compact_count(n: i64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1} mil", n as f64 / 1e3).replace('.', ",")
    } else {
        format!("{:.1} mi", n as f64 / 1e6).replace('.', ",")
    }
}

impl App {
    fn palette_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        if let Some(s) = self.session.as_ref().filter(|s| s.ready) {
            for t in &s.tables {
                let rows = t
                    .estimate
                    .map(|n| format!(" · ~{} linhas", format_count(n)))
                    .unwrap_or_default();
                items.push(Item {
                    icon: if t.is_view { icon::EYE } else { icon::TABLE },
                    label: t.name.clone(),
                    detail: format!("{}{rows}", t.schema),
                    cmd: Cmd::OpenTable(t.schema.clone(), t.name.clone()),
                });
            }
            items.push(Item {
                icon: icon::CODE,
                label: "Novo editor SQL".into(),
                detail: "Ctrl+T".into(),
                cmd: Cmd::NewSql,
            });
            for f in favorites::for_conn(&self.favorites, &s.cfg.id) {
                items.push(Item {
                    icon: icon::STAR,
                    label: f.name.clone(),
                    detail: "favorito".into(),
                    cmd: Cmd::OpenSql(f.sql.clone()),
                });
            }
            let now = history::now();
            for e in self
                .history
                .iter()
                .rev()
                .filter(|e| e.conn == s.cfg.name)
                .take(30)
            {
                let first: String = e
                    .sql
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(70)
                    .collect();
                items.push(Item {
                    icon: icon::CLOCK_COUNTER_CLOCKWISE,
                    label: first,
                    detail: history::ago(e.at, now),
                    cmd: Cmd::OpenSql(e.sql.clone()),
                });
            }
        }
        for c in &self.saved {
            items.push(Item {
                icon: icon::DATABASE,
                label: format!("Conectar: {}", c.name),
                detail: format!("{} · {}:{}", c.kind.label(), c.host, c.port),
                cmd: Cmd::Connect(c.id.clone()),
            });
        }
        items.push(Item {
            icon: icon::PLUS,
            label: "Nova conexão".into(),
            detail: String::new(),
            cmd: Cmd::NewConnection,
        });
        if self.session.is_some() {
            items.push(Item {
                icon: icon::PLUGS,
                label: "Desconectar".into(),
                detail: String::new(),
                cmd: Cmd::Disconnect,
            });
        }
        items
    }

    pub fn palette_ui(&mut self, ctx: &egui::Context) {
        let cache_id = egui::Id::new("palette_results");
        if let Some(p) = &mut self.palette
            && !p.started
        {
            p.started = true;
            ctx.data_mut(|d| d.remove::<(String, Vec<Item>)>(cache_id));
        }
        let Some(p) = &self.palette else { return };
        // Searching every table each frame adds up on large schemas: only redo it when the query
        // changes (the palette is modal, so nothing else changes underneath it meanwhile).
        let cached = ctx
            .data(|d| d.get_temp::<(String, Vec<Item>)>(cache_id))
            .filter(|(q, _)| *q == p.query);
        let matches = match cached {
            Some((_, items)) => items,
            None => {
                let mut scored: Vec<(i32, Item)> = self
                    .palette_items()
                    .into_iter()
                    .filter_map(|it| fuzzy_score(&p.query, &it.label).map(|s| (s, it)))
                    .collect();
                scored.sort_by_key(|m| std::cmp::Reverse(m.0));
                scored.truncate(MAX_RESULTS);
                let items: Vec<Item> = scored.into_iter().map(|(_, it)| it).collect();
                ctx.data_mut(|d| d.insert_temp(cache_id, (p.query.clone(), items.clone())));
                items
            }
        };

        let t = theme::tokens();
        let mut run: Option<Cmd> = None;
        let Some(p) = &mut self.palette else { return };
        p.selected = p.selected.min(matches.len().saturating_sub(1));
        let frame = theme::modal_frame(8);
        let id = egui::Id::new("palette");
        // Near the top, like editor command palettes, so results grow downward.
        let area = egui::Modal::default_area(id).anchor(egui::Align2::CENTER_TOP, [0.0, 72.0]);
        let modal = egui::Modal::new(id).area(area).frame(frame).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.horizontal(|ui| {
                ui.label(theme::icon(icon::MAGNIFYING_GLASS).color(t.muted_foreground));
                let r = ui.add(
                    TextEdit::singleline(&mut p.query)
                        .hint_text("Buscar tabela, conexão ou ação…")
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
                if r.changed() {
                    p.selected = 0;
                }
            });
            ui.add_space(4.0);
            ui.painter().hline(
                ui.max_rect().x_range(),
                ui.cursor().top(),
                egui::Stroke::new(1.0, t.border),
            );
            ui.add_space(4.0);
            if matches.is_empty() {
                ui.label(egui::RichText::new("Nada encontrado").color(t.subtle));
            }
            ui.input(|i| {
                if i.key_pressed(Key::ArrowDown) {
                    p.selected = (p.selected + 1).min(matches.len().saturating_sub(1));
                }
                if i.key_pressed(Key::ArrowUp) {
                    p.selected = p.selected.saturating_sub(1);
                }
                if i.key_pressed(Key::Enter) {
                    run = matches.get(p.selected).map(|it| it.cmd.clone());
                }
            });
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, it) in matches.iter().enumerate() {
                let selected = i == p.selected;
                let (rect, r) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
                if r.hovered() {
                    p.selected = i;
                }
                if r.clicked() {
                    run = Some(it.cmd.clone());
                }
                let painter = ui.painter();
                if selected {
                    painter.rect_filled(rect, 0.0, t.selection);
                    painter.vline(rect.left() + 1.0, rect.y_range(), egui::Stroke::new(2.0, t.lime));
                }
                let mid = rect.center().y;
                let ic = if selected { t.lime } else { t.subtle };
                painter.text(
                    egui::pos2(rect.left() + 12.0, mid),
                    egui::Align2::LEFT_CENTER,
                    it.icon,
                    egui::FontId::new(14.0, theme::icon_family()),
                    ic,
                );
                painter.text(
                    egui::pos2(rect.left() + 36.0, mid),
                    egui::Align2::LEFT_CENTER,
                    &it.label,
                    egui::FontId::proportional(13.0),
                    if selected {
                        t.foreground
                    } else {
                        t.muted_foreground
                    },
                );
                painter.text(
                    egui::pos2(rect.right() - 12.0, mid),
                    egui::Align2::RIGHT_CENTER,
                    &it.detail,
                    egui::FontId::proportional(11.0),
                    t.subtle,
                );
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("↑↓ navegar · Enter abrir · Esc fechar")
                    .size(11.0)
                    .color(t.subtle),
            );
        });
        if modal.should_close() {
            self.palette = None;
        }

        if let Some(cmd) = run {
            self.palette = None;
            match cmd {
                Cmd::OpenTable(schema, table) => self.open_table(&schema, &table),
                Cmd::Connect(id) => self.connect_saved(&id),
                Cmd::NewSql => self.open_sql(String::new()),
                Cmd::OpenSql(sql) => self.open_sql(sql),
                Cmd::NewConnection => {
                    self.form = Some(ConnForm {
                        editing: None,
                        cfg: ConnConfig::new(Kind::Postgres),
                    })
                }
                Cmd::Disconnect => {
                    self.disconnect();
                }
            }
        }
    }

    pub fn open_palette(&mut self) {
        self.palette = Some(Palette::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_ranks_prefix_and_runs() {
        assert!(fuzzy_score("cli", "clientes").unwrap() > fuzzy_score("cli", "pedidos_clientes").unwrap());
        assert!(fuzzy_score("users", "users").unwrap() > fuzzy_score("users", "users_archive").unwrap());
        assert!(fuzzy_score("pc", "pedidos_clientes").is_some());
        assert!(fuzzy_score("xyz", "clientes").is_none());
    }

    #[test]
    fn counts_use_thousand_separators() {
        assert_eq!(format_count(5312), "5.312");
        assert_eq!(format_count(1_234_567), "1.234.567");
        assert_eq!(format_count(42), "42");
        assert_eq!(compact_count(950), "950");
        assert_eq!(compact_count(1_250), "1,2 mil");
        assert_eq!(compact_count(3_400_000), "3,4 mi");
    }
}
