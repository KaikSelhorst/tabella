//! Ctrl+P command palette: one search box for tables, connections and actions.

use eframe::egui::{self, Key, TextEdit};

use super::{icon, theme};
use crate::app::{App, ConnForm, Dialog, Palette};
use crate::db::{ConnConfig, Kind};
use crate::favorites;
use crate::history;

const MAX_RESULTS: usize = 12;
/// Columns rank below tables and actions with a similar match.
const COLUMN_PENALTY: i32 = 24;
/// What a recently opened table adds to its score (less for each place down the list): enough to
/// lead on an empty query and to break ties, not to beat a clearly better match.
const RECENT_BONUS: i32 = 40;

#[derive(Clone)]
enum Cmd {
    Activity,
    Theme,
    SystemTheme,
    RowLimit,
    Back,
    ExportConnections,
    ImportConnections,
    Backup,
    Restore,
    Fps,
    /// +1 / -1 steps; 0 back to 100%.
    Zoom(i32),
    History,
    RunFile,
    Dump,
    Compare,
    Diagram,
    Search,
    NewTable,
    Help,
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
    // The digits in threes; the sign isn't one of them ("-123", not "-,123").
    let s = n.unsigned_abs().to_string();
    let mut out = String::from(if n < 0 { "-" } else { "" });
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Bytes as "48 kB", "120 MB", "1.2 GB".
pub fn format_bytes(b: i64) -> String {
    let b = b as f64;
    let (v, unit) = if b < 1024.0 * 1024.0 {
        (b / 1024.0, "kB")
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        (b / 1024.0 / 1024.0, "MB")
    } else {
        (b / 1024.0 / 1024.0 / 1024.0, "GB")
    };
    if v < 10.0 {
        format!("{v:.1} {unit}")
    } else {
        format!("{v:.0} {unit}")
    }
}

/// Short form for tight spots: 950, 1.2k, 3.4M.
pub fn compact_count(n: i64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        format!("{:.1}M", n as f64 / 1e6)
    }
}

impl App {
    fn palette_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        self.session_items(&mut items);
        self.connection_items(&mut items);
        self.theme_items(&mut items);
        self.file_items(&mut items);
        self.view_items(&mut items);
        self.tool_items(&mut items);
        items
    }

    /// The open connection's tables, a new editor, its favorites and recent queries.
    fn session_items(&self, items: &mut Vec<Item>) {
        if let Some(s) = self.session.as_ref().filter(|s| s.ready) {
            for t in &s.tables {
                let rows = t
                    .estimate
                    .map(|n| format!(" · ~{} rows", format_count(n)))
                    .unwrap_or_default();
                let recent = if self.recent_rank(&t.schema, &t.name).is_some() {
                    "recent · "
                } else {
                    ""
                };
                items.push(Item {
                    icon: if t.is_view { icon::EYE } else { icon::TABLE },
                    label: t.name.clone(),
                    detail: format!("{recent}{}{rows}", t.schema),
                    cmd: Cmd::OpenTable(t.schema.clone(), t.name.clone()),
                });
            }
            items.push(Item {
                icon: icon::CODE,
                label: "New SQL editor".into(),
                detail: "Ctrl+T".into(),
                cmd: Cmd::NewSql,
            });
            for f in favorites::for_conn(&self.library.favorites, &s.cfg.id) {
                items.push(Item {
                    icon: icon::STAR,
                    label: f.name.clone(),
                    detail: "favorite".into(),
                    cmd: Cmd::OpenSql(f.sql.clone()),
                });
            }
            let now = history::now();
            for e in self
                .library
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
    }

    /// A "Connect" entry per saved connection.
    fn connection_items(&self, items: &mut Vec<Item>) {
        for c in &self.connections.saved {
            items.push(Item {
                icon: icon::DATABASE,
                label: format!("Connect: {}", c.name),
                detail: {
                    let place = match c.kind {
                        crate::db::Kind::Sqlite => format!("SQLite · {}", c.database),
                        _ => format!("{} · {}:{}", c.kind.label(), c.host, c.port),
                    };
                    match c.group.trim() {
                        "" => place,
                        group => format!("{group} · {place}"),
                    }
                },
                cmd: Cmd::Connect(c.id.clone()),
            });
        }
    }

    /// Light or dark, and following the system.
    fn theme_items(&self, items: &mut Vec<Item>) {
        let (theme_icon, theme_label) = if self.settings.light {
            (icon::MOON, "Dark theme")
        } else {
            (icon::SUN, "Light theme")
        };
        items.push(Item {
            icon: theme_icon,
            label: theme_label.into(),
            detail: "switch the theme".into(),
            cmd: Cmd::Theme,
        });
        items.push(Item {
            icon: icon::DESKTOP,
            label: if self.settings.system_theme {
                "Stop following the system theme"
            } else {
                "Follow the system theme"
            }
            .into(),
            detail: "light or dark with the operating system".into(),
            cmd: Cmd::SystemTheme,
        });
    }

    /// Backups, connection files and "Go back".
    fn file_items(&self, items: &mut Vec<Item>) {
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.ready && s.cfg.kind != crate::db::Kind::Sqlite)
        {
            items.push(Item {
                icon: icon::FLOPPY_DISK,
                label: "Back up database".into(),
                detail: "with pg_dump / mysqldump".into(),
                cmd: Cmd::Backup,
            });
            items.push(Item {
                icon: icon::ARROW_COUNTER_CLOCKWISE,
                label: "Restore database".into(),
                detail: "a backup, with psql / pg_restore / mysql".into(),
                cmd: Cmd::Restore,
            });
        }
        items.push(Item {
            icon: icon::EXPORT,
            label: "Export connections".into(),
            detail: "a JSON file without passwords".into(),
            cmd: Cmd::ExportConnections,
        });
        items.push(Item {
            icon: icon::DOWNLOAD_SIMPLE,
            label: "Import connections".into(),
            detail: "from a file made by Export connections".into(),
            cmd: Cmd::ImportConnections,
        });
        if self.can_go_back() {
            items.push(Item {
                icon: icon::ARROW_LEFT,
                label: "Go back".into(),
                detail: "Alt+← · to where the last foreign key was followed from".into(),
                cmd: Cmd::Back,
            });
        }
    }

    /// Row limit, zoom, FPS counter, shortcuts and a new connection.
    fn view_items(&self, items: &mut Vec<Item>) {
        items.push(Item {
            icon: icon::LIST_NUMBERS,
            label: "Row limit".into(),
            detail: format!(
                "now {} rows per query · pick the next",
                format_count(self.settings.row_limit as i64)
            ),
            cmd: Cmd::RowLimit,
        });
        let zoom = format!("now {:.0}%", self.settings.zoom * 100.0);
        for (steps, glyph, label, keys) in [
            (1, icon::MAGNIFYING_GLASS_PLUS, "Zoom in", "Ctrl +"),
            (-1, icon::MAGNIFYING_GLASS_MINUS, "Zoom out", "Ctrl −"),
            (0, icon::MAGNIFYING_GLASS, "Zoom 100%", "Ctrl 0"),
        ] {
            items.push(Item {
                icon: glyph,
                label: label.into(),
                detail: format!("{keys} · {zoom}"),
                cmd: Cmd::Zoom(steps),
            });
        }
        items.push(Item {
            icon: icon::GAUGE,
            label: if self.fps.show {
                "Hide FPS counter"
            } else {
                "Show FPS counter"
            }
            .into(),
            detail: "diagnostics: click the counter to repaint at the monitor's rate".into(),
            cmd: Cmd::Fps,
        });
        items.push(Item {
            icon: icon::KEYBOARD,
            label: "Keyboard shortcuts".into(),
            detail: "F1".into(),
            cmd: Cmd::Help,
        });
        items.push(Item {
            icon: icon::PLUS,
            label: "New connection".into(),
            detail: String::new(),
            cmd: Cmd::NewConnection,
        });
    }

    /// What needs a connection: history, diagram, search, scripts, compare, dump.
    fn tool_items(&self, items: &mut Vec<Item>) {
        if self.session.is_some() {
            items.push(Item {
                icon: icon::CLOCK_COUNTER_CLOCKWISE,
                label: "Query history".into(),
                detail: "Ctrl+Shift+H".into(),
                cmd: Cmd::History,
            });
            items.push(Item {
                icon: icon::GRAPH,
                label: "ER diagram".into(),
                detail: "tables and foreign keys (right-click a schema for another one)".into(),
                cmd: Cmd::Diagram,
            });
            items.push(Item {
                icon: icon::MAGNIFYING_GLASS,
                label: "Search value in all tables".into(),
                detail: "Ctrl+Shift+F".into(),
                cmd: Cmd::Search,
            });
            if !self.session.as_ref().is_some_and(|s| s.cfg.read_only) {
                items.push(Item {
                    icon: icon::PLAY,
                    label: "Run .sql file…".into(),
                    detail:
                        "dump or large migration, statement by statement, without opening it in the editor"
                            .into(),
                    cmd: Cmd::RunFile,
                });
                items.push(Item {
                    icon: icon::PLUS,
                    label: "New table".into(),
                    detail: "columns, primary key and indexes through a form".into(),
                    cmd: Cmd::NewTable,
                });
            }
            items.push(Item {
                icon: icon::GIT_DIFF,
                label: "Compare structure…".into(),
                detail: "with another schema or database (dev × production): differences and the SQL to match them".into(),
                cmd: Cmd::Compare,
            });
            items.push(Item {
                icon: icon::DOWNLOAD_SIMPLE,
                label: "Export database as .sql…".into(),
                detail: "structure and data of every table, views, functions and triggers".into(),
                cmd: Cmd::Dump,
            });
            items.push(Item {
                icon: icon::PULSE,
                label: "Server activity".into(),
                detail: "sessions and running queries".into(),
                cmd: Cmd::Activity,
            });
            items.push(Item {
                icon: icon::PLUGS,
                label: "Disconnect".into(),
                detail: String::new(),
                cmd: Cmd::Disconnect,
            });
        }
    }

    /// The items matching `query`, best first: tables (recent ones higher), connections and
    /// actions, then columns from two letters on.
    fn palette_matches(&self, query: &str) -> Vec<Item> {
        let mut scored: Vec<(i32, Item)> = self
            .palette_items()
            .into_iter()
            .filter_map(|it| {
                let mut score = fuzzy_score(query, &it.label)?;
                if let Cmd::OpenTable(schema, table) = &it.cmd
                    && let Some(rank) = self.recent_rank(schema, table)
                {
                    score += RECENT_BONUS - rank as i32;
                }
                Some((score, it))
            })
            .collect();
        // Columns ("where is the ssn column?") from two letters on, below tables and actions
        // that match as well.
        if query.trim().chars().count() >= 2
            && let Some(s) = self.session.as_ref().filter(|s| s.ready)
        {
            scored.extend(s.schema_cols.iter().filter_map(|c| {
                let label = format!("{}.{}", c.table, c.column);
                let score = fuzzy_score(query, &label)?;
                Some((
                    score - COLUMN_PENALTY,
                    Item {
                        icon: icon::COLUMNS,
                        label,
                        detail: format!("column · {} · {}", c.data_type, c.schema),
                        cmd: Cmd::OpenTable(c.schema.clone(), c.table.clone()),
                    },
                ))
            }));
        }
        scored.sort_by_key(|m| std::cmp::Reverse(m.0));
        scored.truncate(MAX_RESULTS);
        let items: Vec<Item> = scored.into_iter().map(|(_, it)| it).collect();
        items
    }

    /// Carries out what was picked in the palette.
    fn run_command(&mut self, ctx: &egui::Context, cmd: Cmd) {
        match cmd {
            Cmd::OpenTable(schema, table) => self.open_table(&schema, &table),
            Cmd::Connect(id) => self.connect_saved(&id),
            Cmd::NewSql => self.open_sql(String::new()),
            Cmd::OpenSql(sql) => self.open_sql(sql),
            Cmd::NewConnection => {
                self.connections.form = Some(ConnForm {
                    editing: None,
                    cfg: ConnConfig::new(Kind::Postgres),
                })
            }
            Cmd::Activity => self.open_activity(),
            Cmd::Theme => self.toggle_theme(ctx),
            Cmd::SystemTheme => self.toggle_system_theme(ctx),
            Cmd::RowLimit => self.cycle_row_limit(ctx),
            Cmd::Back => self.go_back(),
            Cmd::Backup => self.backup_dialog(false),
            Cmd::Restore => self.backup_dialog(true),
            Cmd::ExportConnections => self.export_connections_dialog(),
            Cmd::ImportConnections => self.import_connections_dialog(),
            Cmd::Zoom(steps) => self.zoom_by(ctx, steps),
            Cmd::History => self.open_history(),
            Cmd::RunFile => self.pick_script(),
            Cmd::Fps => {
                self.fps.show = !self.fps.show;
                // Hidden, it shouldn't keep redrawing at full rate either.
                self.fps.max &= self.fps.show;
            }
            Cmd::Dump => self.open_dump(crate::app::DumpScope::Database),
            Cmd::Compare => self.open_compare(None),
            Cmd::Diagram => self.open_diagram(None),
            Cmd::Search => self.open_search(),
            Cmd::NewTable => self.open_new_table(None),
            Cmd::Help => self.dialog = Some(Dialog::Help),
            Cmd::Disconnect => {
                self.disconnect();
            }
        }
    }

    pub fn palette_ui(&mut self, ctx: &egui::Context) {
        let cache_id = egui::Id::new("palette_results");
        if let Some(Dialog::Palette(p)) = &mut self.dialog
            && !p.started
        {
            p.started = true;
            ctx.data_mut(|d| d.remove::<(String, Vec<Item>)>(cache_id));
        }
        let Some(Dialog::Palette(p)) = &self.dialog else {
            return;
        };
        // Searching every table each frame adds up on large schemas: only redo it when the query
        // changes (the palette is modal, so nothing else changes underneath it meanwhile).
        let cached = ctx
            .data(|d| d.get_temp::<(String, Vec<Item>)>(cache_id))
            .filter(|(q, _)| *q == p.query);
        let matches = match cached {
            Some((_, items)) => items,
            None => {
                let items = self.palette_matches(&p.query);
                ctx.data_mut(|d| d.insert_temp(cache_id, (p.query.clone(), items.clone())));
                items
            }
        };

        let t = theme::tokens();
        let mut run: Option<Cmd> = None;
        let Some(Dialog::Palette(p)) = &mut self.dialog else {
            return;
        };
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
                        .hint_text("Search table, connection or action…")
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
                ui.label(egui::RichText::new("Nothing found").color(t.subtle));
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
                result_row(ui.painter(), rect, it, selected);
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("↑↓ navigate · Enter open · Esc close")
                    .size(11.0)
                    .color(t.subtle),
            );
        });
        if modal.should_close() {
            self.dialog = None;
        }

        if let Some(cmd) = run {
            self.dialog = None;
            self.run_command(ctx, cmd);
        }
    }

    pub fn open_palette(&mut self) {
        self.dialog = Some(Dialog::Palette(Palette::default()));
    }
}

/// One result: its icon, label and detail, lit when selected.
fn result_row(painter: &egui::Painter, rect: egui::Rect, it: &Item, selected: bool) {
    let t = theme::tokens();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_ranks_prefix_and_runs() {
        assert!(fuzzy_score("cus", "customers").unwrap() > fuzzy_score("cus", "orders_customers").unwrap());
        assert!(fuzzy_score("users", "users").unwrap() > fuzzy_score("users", "users_archive").unwrap());
        assert!(fuzzy_score("pc", "pedidos_clientes").is_some());
        assert!(fuzzy_score("xyz", "customers").is_none());
    }

    #[test]
    fn counts_use_thousand_separators() {
        assert_eq!(format_count(5312), "5,312");
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(format_count(42), "42");
        assert_eq!(compact_count(950), "950");
        assert_eq!(format_bytes(49_152), "48 kB");
        assert_eq!(format_bytes(1_258_291), "1.2 MB");
        assert_eq!(format_bytes(126 * 1024 * 1024), "126 MB");
        assert_eq!(compact_count(1_250), "1.2k");
        assert_eq!(compact_count(3_400_000), "3.4M");
    }

    /// Ctrl+P scores every name against what is typed, at every keystroke.
    #[test]
    fn fuzzy_scores_take_any_text() {
        crate::testgen::each_text(28, 3000, |text, g| {
            let query = g.text_of(2);
            let _ = (
                fuzzy_score(&query, text),
                fuzzy_score(text, &query),
                fuzzy_score(text, text),
            );
        });
    }

    #[test]
    fn counts_and_sizes_at_the_extremes() {
        for n in [
            i64::MIN,
            -1_234_567,
            -123,
            -1,
            0,
            999,
            1_000,
            999_999,
            1_000_000,
            i64::MAX,
        ] {
            let _ = (format_bytes(n), compact_count(n));
            let s = format_count(n);
            // Digits in threes after the sign, and nothing else changed.
            assert_eq!(s.replace(',', ""), n.to_string());
            assert!(!s.starts_with(',') && !s.starts_with("-,"), "{s}");
        }
        assert_eq!(format_count(-1_234_567), "-1,234,567");
        assert_eq!(format_count(-123), "-123");
    }
}
