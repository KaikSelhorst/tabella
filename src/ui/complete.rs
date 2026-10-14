//! SQL editor autocomplete: tables, columns and keywords, ranked by what the cursor follows.

use eframe::egui::{self, Key, Modifiers, TextEdit, text::CCursor, text::CCursorRange};

use super::{icon, syntax::KEYWORDS, theme};
use crate::db::TableRef;

const MAX_ITEMS: usize = 8;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ItemKind {
    Table,
    Column,
    Keyword,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub text: String,
    pub kind: ItemKind,
    /// Where it comes from, e.g. the table of a column.
    pub detail: String,
}

#[derive(Debug, PartialEq)]
pub struct Suggestions {
    /// Char index where the word being completed starts.
    pub start: usize,
    pub items: Vec<Item>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The last keyword before `pos`, uppercased, to guess whether a table or a column comes next.
fn previous_keyword(chars: &[char], pos: usize) -> Option<String> {
    let before: String = chars[..pos].iter().collect();
    before
        .split(|c: char| !is_word(c))
        .rev()
        .filter(|w| !w.is_empty())
        .map(str::to_uppercase)
        .find(|w| KEYWORDS.contains(&w.as_str()))
}

pub fn suggest(
    text: &str,
    cursor: usize,
    tables: &[TableRef],
    columns: &[(String, String)],
) -> Option<Suggestions> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    // Don't pop up in the middle of a word.
    if chars.get(cursor).is_some_and(|c| is_word(*c)) {
        return None;
    }
    let mut start = cursor;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let prefix: String = chars[start..cursor].iter().collect();
    let lower = prefix.to_lowercase();
    // After `name.` only that table's columns make sense (works for real names; aliases fall back to all).
    let qualifier = (start > 0 && chars[start - 1] == '.').then(|| {
        let mut q = start - 1;
        while q > 0 && is_word(chars[q - 1]) {
            q -= 1;
        }
        chars[q..start - 1].iter().collect::<String>().to_lowercase()
    });
    if prefix.is_empty() && qualifier.is_none() {
        return None;
    }
    let matches = |s: &str| {
        let l = s.to_lowercase();
        l.starts_with(&lower) && l != lower
    };

    let mut table_items: Vec<Item> = Vec::new();
    let mut column_items: Vec<Item> = Vec::new();
    let mut keyword_items: Vec<Item> = Vec::new();
    let qualified_cols: Vec<&(String, String)> = match &qualifier {
        Some(q) if columns.iter().any(|(t, _)| t.to_lowercase() == *q) => {
            columns.iter().filter(|(t, _)| t.to_lowercase() == *q).collect()
        }
        _ => columns.iter().collect(),
    };
    for (t, c) in qualified_cols {
        if matches(c) && !column_items.iter().any(|i| &i.text == c) {
            column_items.push(Item {
                text: c.clone(),
                kind: ItemKind::Column,
                detail: t.clone(),
            });
        }
    }
    if qualifier.is_none() {
        for t in tables {
            if matches(&t.name) && !table_items.iter().any(|i| i.text == t.name) {
                table_items.push(Item {
                    text: t.name.clone(),
                    kind: ItemKind::Table,
                    detail: t.schema.clone(),
                });
            }
        }
        // Keep the user's casing style for keywords.
        let upper = prefix.chars().next().is_some_and(char::is_uppercase);
        for k in KEYWORDS {
            if matches(k) {
                let text = if upper { k.to_string() } else { k.to_lowercase() };
                keyword_items.push(Item {
                    text,
                    kind: ItemKind::Keyword,
                    detail: String::new(),
                });
            }
        }
    }
    let wants_table = matches!(
        previous_keyword(&chars, start).as_deref(),
        Some("FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE")
    );
    let mut items = if wants_table {
        [table_items, column_items, keyword_items].concat()
    } else {
        [column_items, table_items, keyword_items].concat()
    };
    items.truncate(MAX_ITEMS);
    (!items.is_empty()).then_some(Suggestions { start, items })
}

/// Popup state kept in egui memory per editor.
#[derive(Clone, Default)]
struct State {
    selected: usize,
    /// Escape hides the popup until the cursor moves to another word.
    dismissed_at: Option<usize>,
}

/// Call before drawing the editor: handles the popup keys (so the editor doesn't also get them)
/// and applies an accepted suggestion to `text`. Returns what to draw after the editor.
pub fn before_editor(
    ctx: &egui::Context,
    editor_id: egui::Id,
    text: &mut String,
    tables: &[TableRef],
    columns: &[(String, String)],
) -> Option<(Suggestions, usize)> {
    if !ctx.memory(|m| m.has_focus(editor_id)) {
        return None;
    }
    let mut te = TextEdit::load_state(ctx, editor_id)?;
    let range = te.cursor.char_range()?;
    if range.primary != range.secondary {
        return None;
    }
    let cursor = usize::from(range.primary.index);
    let sugg = suggest(text, cursor, tables, columns)?;
    let state_id = editor_id.with("complete");
    let mut st: State = ctx.data(|d| d.get_temp(state_id)).unwrap_or_default();
    if st.dismissed_at == Some(sugg.start) {
        return None;
    }
    st.selected = st.selected.min(sugg.items.len() - 1);
    let (down, up, accept, escape) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Tab) || i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Escape),
        )
    });
    if down {
        st.selected = (st.selected + 1) % sugg.items.len();
    }
    if up {
        st.selected = (st.selected + sugg.items.len() - 1) % sugg.items.len();
    }
    if escape {
        st.dismissed_at = Some(sugg.start);
        ctx.data_mut(|d| d.insert_temp(state_id, st));
        return None;
    }
    if accept {
        let item = &sugg.items[st.selected];
        let byte = |ci: usize| text.char_indices().nth(ci).map_or(text.len(), |(b, _)| b);
        let (a, b) = (byte(sugg.start), byte(cursor));
        text.replace_range(a..b, &item.text);
        let pos = sugg.start + item.text.chars().count();
        te.cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(pos))));
        te.store(ctx, editor_id);
        ctx.data_mut(|d| d.insert_temp(state_id, State::default()));
        return None;
    }
    let selected = st.selected;
    ctx.data_mut(|d| d.insert_temp(state_id, st));
    Some((sugg, selected))
}

/// Draws the suggestion list just under the cursor.
pub fn popup(
    ctx: &egui::Context,
    editor_id: egui::Id,
    anchor: egui::Pos2,
    sugg: &Suggestions,
    selected: usize,
) {
    let t = theme::tokens();
    egui::Area::new(editor_id.with("complete_popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(anchor)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(t.card)
                .stroke(egui::Stroke::new(1.0, t.border))
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (i, it) in sugg.items.iter().enumerate() {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(300.0, 22.0), egui::Sense::hover());
                        let p = ui.painter();
                        if i == selected {
                            p.rect_filled(rect, 0.0, t.selection);
                        }
                        let (glyph, color) = match it.kind {
                            ItemKind::Table => (icon::TABLE, t.info),
                            ItemKind::Column => (icon::COLUMNS, t.number),
                            ItemKind::Keyword => (icon::TEXT_AA, t.subtle),
                        };
                        p.text(
                            rect.left_center() + egui::vec2(8.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            glyph,
                            egui::FontId::new(12.0, theme::icon_family()),
                            color,
                        );
                        p.text(
                            rect.left_center() + egui::vec2(28.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            &it.text,
                            egui::FontId::monospace(12.0),
                            if i == selected {
                                t.foreground
                            } else {
                                t.muted_foreground
                            },
                        );
                        p.text(
                            rect.right_center() - egui::vec2(8.0, 0.0),
                            egui::Align2::RIGHT_CENTER,
                            &it.detail,
                            egui::FontId::proportional(11.0),
                            t.subtle,
                        );
                    }
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("Tab/Enter aceita · Esc fecha")
                            .size(10.0)
                            .color(t.subtle),
                    );
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables() -> Vec<TableRef> {
        ["clientes", "pedidos", "produtos"]
            .iter()
            .map(|n| TableRef {
                schema: "public".into(),
                name: n.to_string(),
                is_view: false,
                estimate: None,
                search_key: n.to_string(),
                size: None,
            })
            .collect()
    }

    fn columns() -> Vec<(String, String)> {
        vec![
            ("clientes".into(), "id".into()),
            ("clientes".into(), "nome".into()),
            ("pedidos".into(), "id".into()),
            ("pedidos".into(), "produto_id".into()),
        ]
    }

    fn texts(s: &Option<Suggestions>) -> Vec<&str> {
        s.as_ref()
            .map(|s| s.items.iter().map(|i| i.text.as_str()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn tables_first_after_from() {
        let text = "SELECT * FROM p";
        let s = suggest(text, text.len(), &tables(), &columns());
        assert_eq!(texts(&s)[..2], ["pedidos", "produtos"]);
        assert_eq!(s.unwrap().start, 14);
    }

    #[test]
    fn columns_first_elsewhere_and_keywords_keep_case() {
        let text = "SELECT p";
        assert_eq!(
            texts(&suggest(text, text.len(), &tables(), &columns()))[0],
            "produto_id"
        );
        let text = "sel";
        assert_eq!(texts(&suggest(text, 3, &tables(), &columns())), ["select"]);
    }

    #[test]
    fn qualified_columns_only() {
        let text = "SELECT clientes.";
        assert_eq!(
            texts(&suggest(text, text.len(), &tables(), &columns())),
            ["id", "nome"]
        );
    }

    #[test]
    fn quiet_when_nothing_to_do() {
        assert!(suggest("SELECT ", 7, &tables(), &columns()).is_none());
        assert!(suggest("clientes", 3, &tables(), &columns()).is_none()); // mid-word
        assert!(suggest("clientes", 8, &tables(), &columns()).is_none()); // exact match only
    }
}
