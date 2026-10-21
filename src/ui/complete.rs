//! SQL editor autocomplete: tables, columns and keywords, ranked by what the cursor follows.

use eframe::egui::{self, Key, Modifiers, TextEdit, text::CCursor, text::CCursorRange};

use super::{icon, syntax::KEYWORDS, theme};
use crate::db::{FkEdge, TableRef};

const MAX_ITEMS: usize = 8;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ItemKind {
    Table,
    Column,
    Keyword,
    /// Expands into a template; see `SNIPPETS`.
    Snippet,
    /// `table t ON t.id = x.table_id`, from a foreign key.
    Join,
}

/// Where the cursor lands after a snippet expands.
const CURSOR: char = '¦';

/// `(trigger, what it is, template)`: typing the trigger offers the template in the completion
/// list.
pub const SNIPPETS: &[(&str, &str, &str)] = &[
    ("sel", "SELECT … LIMIT 100", "SELECT *\nFROM ¦\nLIMIT 100;"),
    ("selw", "SELECT … WHERE …", "SELECT *\nFROM ¦\nWHERE \nLIMIT 100;"),
    ("cnt", "count rows", "SELECT count(*)\nFROM ¦;"),
    (
        "grp",
        "group and count",
        "SELECT ¦, count(*)\nFROM \nGROUP BY 1\nORDER BY 2 DESC;",
    ),
    (
        "dup",
        "duplicate values",
        "SELECT ¦, count(*)\nFROM \nGROUP BY 1\nHAVING count(*) > 1\nORDER BY 2 DESC;",
    ),
    ("join", "JOIN … ON", "JOIN ¦ ON  = "),
    ("ljoin", "LEFT JOIN … ON", "LEFT JOIN ¦ ON  = "),
    (
        "cte",
        "WITH … AS (…)",
        "WITH base AS (\n  ¦\n)\nSELECT *\nFROM base;",
    ),
    ("ins", "INSERT INTO … VALUES", "INSERT INTO ¦ ()\nVALUES ();"),
    ("upd", "UPDATE … SET … WHERE", "UPDATE ¦\nSET  = \nWHERE ;"),
    ("del", "DELETE … WHERE", "DELETE FROM ¦\nWHERE ;"),
];

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

/// What the editor knows about the database, for suggestions.
#[derive(Clone, Copy)]
pub struct Catalog<'a> {
    pub tables: &'a [TableRef],
    /// `(table, column)`.
    pub columns: &'a [(String, String)],
    pub fks: &'a [FkEdge],
}

/// `(alias or table name, table)` for each table the query names after FROM / JOIN / UPDATE /
/// INTO, lowercase: `FROM orders p JOIN customers AS c` → (p, orders), (c, customers); a table
/// without alias answers to its own name.
pub fn query_tables(text: &str) -> Vec<(String, String)> {
    // Words (identifiers, possibly dotted and quoted) and single punctuation chars.
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() || matches!(c, '_' | '.' | '"' | '`') {
            cur.push(c);
        } else {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
            if c == ',' || c == ';' || c == '(' || c == ')' {
                tokens.push(c.to_string());
            }
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let is_kw = |w: &str| KEYWORDS.contains(&w.to_uppercase().as_str());
    let name_of = |w: &str| {
        w.rsplit('.')
            .next()
            .unwrap_or(w)
            .trim_matches(|c| c == '"' || c == '`')
            .to_lowercase()
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let up = tokens[i].to_uppercase();
        if !matches!(up.as_str(), "FROM" | "JOIN" | "UPDATE" | "INTO") {
            i += 1;
            continue;
        }
        // FROM a x, b y: a list.
        loop {
            i += 1;
            let Some(t) = tokens.get(i) else { break };
            // A subquery, the end of the list, or the next clause.
            if matches!(t.as_str(), "(" | ")" | "," | ";") || is_kw(t) {
                break;
            }
            let table = name_of(t);
            let mut alias = table.clone();
            let mut j = i + 1;
            if tokens.get(j).is_some_and(|w| w.eq_ignore_ascii_case("AS")) {
                j += 1;
            }
            if let Some(a) = tokens
                .get(j)
                .filter(|a| !is_kw(a) && a.chars().all(|c| c.is_alphanumeric() || c == '_'))
            {
                alias = a.to_lowercase();
                i = j;
            }
            out.push((alias, table));
            if tokens.get(i + 1).is_some_and(|w| w == ",") {
                i += 1;
                continue;
            }
            break;
        }
    }
    out
}

/// Short alias for a table, not among `used`: initials of its words (`pedido_itens` → `pi`),
/// then a number.
fn new_alias(table: &str, used: &[String]) -> String {
    let base: String = table
        .split('_')
        .filter_map(|p| p.chars().next())
        .collect::<String>()
        .to_lowercase();
    let base = if base.is_empty() { "t".to_string() } else { base };
    if !used.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}{n}"))
        .find(|a| !used.contains(a))
        .unwrap_or(base)
}

/// After JOIN: the tables linked by a foreign key to those already in the query, with the
/// ON condition written out.
fn join_items(text: &str, prefix: &str, fks: &[FkEdge]) -> Vec<Item> {
    let in_query = query_tables(text);
    let used: Vec<String> = in_query.iter().map(|(a, _)| a.clone()).collect();
    let mut out = Vec::new();
    for (alias, table) in &in_query {
        for e in fks {
            let (target, target_col, own_col) = if e.table.to_lowercase() == *table {
                (&e.ref_table, &e.ref_column, &e.column)
            } else if e.ref_table.to_lowercase() == *table {
                (&e.table, &e.column, &e.ref_column)
            } else {
                continue;
            };
            if !target.to_lowercase().starts_with(&prefix.to_lowercase()) {
                continue;
            }
            let a = new_alias(target, &used);
            let text = format!("{target} {a} ON {a}.{target_col} = {alias}.{own_col}");
            if !out.iter().any(|i: &Item| i.text == text) {
                out.push(Item {
                    text,
                    kind: ItemKind::Join,
                    detail: "via the foreign key".into(),
                });
            }
        }
    }
    out
}

pub fn suggest(text: &str, cursor: usize, cat: Catalog) -> Option<Suggestions> {
    let Catalog { tables, columns, fks } = cat;
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
    let after_join = previous_keyword(&chars, start).as_deref() == Some("JOIN");
    // Right after "JOIN ": the joins the foreign keys allow, nothing else.
    if prefix.is_empty() && qualifier.is_none() {
        let words_before: String = chars[..start].iter().collect();
        let last_word = words_before.split_whitespace().last().unwrap_or("");
        if after_join
            && last_word.eq_ignore_ascii_case("JOIN")
            && start > 0
            && chars[start - 1].is_whitespace()
        {
            let mut items = join_items(text, "", fks);
            items.truncate(MAX_ITEMS);
            return (!items.is_empty()).then_some(Suggestions { start, items });
        }
        return None;
    }
    let matches = |s: &str| {
        let l = s.to_lowercase();
        l.starts_with(&lower) && l != lower
    };

    let mut table_items: Vec<Item> = Vec::new();
    let mut column_items: Vec<Item> = Vec::new();
    let mut keyword_items: Vec<Item> = Vec::new();
    // `c.` where c is an alias: that table's columns.
    let qualifier = qualifier.map(|q| {
        query_tables(text)
            .into_iter()
            .find(|(a, _)| *a == q)
            .map_or(q, |(_, t)| t)
    });
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
    // Snippets from two letters on; the one typed exactly comes first.
    let mut exact = Vec::new();
    let mut snippet_items = Vec::new();
    if qualifier.is_none() && lower.chars().count() >= 2 {
        for (trigger, what, _) in SNIPPETS {
            if trigger.starts_with(lower.as_str()) {
                let item = Item {
                    text: trigger.to_string(),
                    kind: ItemKind::Snippet,
                    detail: what.to_string(),
                };
                if *trigger == lower {
                    exact.push(item)
                } else {
                    snippet_items.push(item)
                }
            }
        }
    }
    let joins = if after_join && qualifier.is_none() {
        join_items(text, &prefix, fks)
    } else {
        Vec::new()
    };
    let mut items = if wants_table {
        [
            exact,
            joins,
            table_items,
            column_items,
            keyword_items,
            snippet_items,
        ]
        .concat()
    } else {
        [exact, column_items, table_items, keyword_items, snippet_items].concat()
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
    cat: Catalog,
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
    let sugg = suggest(text, cursor, cat)?;
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
        let (insert, pos) = expansion(item);
        text.replace_range(a..b, &insert);
        let pos = sugg.start + pos;
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

/// Text an accepted item puts in, and where the cursor goes (chars from its start).
fn expansion(item: &Item) -> (String, usize) {
    let template = match item.kind {
        ItemKind::Snippet => SNIPPETS
            .iter()
            .find(|(t, _, _)| *t == item.text)
            .map(|(_, _, s)| *s),
        _ => None,
    };
    match template {
        Some(s) => {
            let pos = s.chars().position(|c| c == CURSOR).unwrap_or(s.chars().count());
            (s.replace(CURSOR, ""), pos)
        }
        None => (item.text.clone(), item.text.chars().count()),
    }
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
                            ItemKind::Snippet => (icon::LIGHTNING, t.lime),
                            ItemKind::Join => (icon::LINK, t.info),
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
                        egui::RichText::new("Tab/Enter accepts · Esc closes")
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
        ["customers", "orders", "products"]
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
            ("customers".into(), "id".into()),
            ("customers".into(), "name".into()),
            ("orders".into(), "id".into()),
            ("orders".into(), "product_id".into()),
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
        let s = suggest(
            text,
            text.len(),
            Catalog {
                tables: &tables(),
                columns: &columns(),
                fks: &[],
            },
        );
        assert_eq!(texts(&s)[..2], ["products", "product_id"]);
        assert_eq!(s.unwrap().start, 14);
    }

    #[test]
    fn columns_first_elsewhere_and_keywords_keep_case() {
        let text = "SELECT p";
        assert_eq!(
            texts(&suggest(
                text,
                text.len(),
                Catalog {
                    tables: &tables(),
                    columns: &columns(),
                    fks: &[]
                }
            ))[0],
            "product_id"
        );
        let text = "sel";
        assert_eq!(
            texts(&suggest(
                text,
                3,
                Catalog {
                    tables: &tables(),
                    columns: &columns(),
                    fks: &[]
                }
            )),
            ["sel", "select", "selw"]
        );
    }

    #[test]
    fn snippets_expand_with_the_cursor_inside() {
        let s = suggest(
            "cn",
            2,
            Catalog {
                tables: &tables(),
                columns: &columns(),
                fks: &[],
            },
        )
        .unwrap();
        let item = s.items.iter().find(|i| i.kind == ItemKind::Snippet).unwrap();
        assert_eq!(item.text, "cnt");
        let (text, pos) = expansion(item);
        assert_eq!(text, "SELECT count(*)\nFROM ;");
        assert_eq!(&text[..pos], "SELECT count(*)\nFROM ");
        // One letter is too little to offer them.
        assert!(
            texts(&suggest(
                "c",
                1,
                Catalog {
                    tables: &tables(),
                    columns: &columns(),
                    fks: &[]
                }
            ))
            .iter()
            .all(|t| *t != "cnt")
        );
    }

    #[test]
    fn aliases_and_joins_by_foreign_key() {
        assert_eq!(
            query_tables(
                "SELECT * FROM public.orders p, products JOIN customers AS c ON c.id = p.customer_id WHERE 1"
            ),
            [
                ("p".to_string(), "orders".to_string()),
                ("products".into(), "products".into()),
                ("c".into(), "customers".into())
            ]
        );
        let cols = columns();
        let fks = [FkEdge {
            schema: "public".into(),
            table: "orders".into(),
            column: "customer_id".into(),
            ref_schema: "public".into(),
            ref_table: "customers".into(),
            ref_column: "id".into(),
        }];
        let cat = Catalog {
            tables: &tables(),
            columns: &cols,
            fks: &fks,
        };
        // `p.` is orders: its columns, not every table's.
        let text = "SELECT p. FROM orders p";
        assert_eq!(texts(&suggest(text, 9, cat)), ["id", "product_id"]);
        // Right after JOIN: the join the foreign key allows, ON clause included.
        let text = "SELECT * FROM orders p JOIN ";
        assert_eq!(
            texts(&suggest(text, text.chars().count(), cat)),
            ["customers c ON c.id = p.customer_id"]
        );
        // From the other side, and the alias avoids the ones in use.
        let text = "SELECT * FROM customers c JOIN ";
        assert_eq!(
            texts(&suggest(text, text.chars().count(), cat)),
            ["orders o ON o.customer_id = c.id"]
        );
        let text = "SELECT * FROM customers o JOIN or";
        assert_eq!(
            texts(&suggest(text, text.chars().count(), cat))[0],
            "orders o2 ON o2.customer_id = o.id"
        );
    }

    #[test]
    fn qualified_columns_only() {
        let text = "SELECT customers.";
        assert_eq!(
            texts(&suggest(
                text,
                text.len(),
                Catalog {
                    tables: &tables(),
                    columns: &columns(),
                    fks: &[]
                }
            )),
            ["id", "name"]
        );
    }

    #[test]
    fn quiet_when_nothing_to_do() {
        assert!(
            suggest(
                "SELECT ",
                7,
                Catalog {
                    tables: &tables(),
                    columns: &columns(),
                    fks: &[]
                }
            )
            .is_none()
        );
        assert!(
            suggest(
                "customers",
                3,
                Catalog {
                    tables: &tables(),
                    columns: &columns(),
                    fks: &[]
                }
            )
            .is_none()
        ); // mid-word
        assert!(
            suggest(
                "customers",
                8,
                Catalog {
                    tables: &tables(),
                    columns: &columns(),
                    fks: &[]
                }
            )
            .is_none()
        ); // exact match only
    }

    /// Completion is asked at every cursor move: any text, any place in it.
    #[test]
    fn any_text_any_cursor() {
        let tables = [crate::db::TableRef {
            schema: "public".into(),
            name: "t".into(),
            is_view: false,
            estimate: None,
            search_key: "t".into(),
            size: None,
        }];
        let columns = [("t".to_string(), "id".to_string())];
        crate::testgen::each_text(10, 2000, |s, g| {
            let n = s.chars().count();
            let _ = query_tables(s);
            for c in [0, n, g.below(n + 1), n + 5] {
                let cat = Catalog {
                    tables: &tables,
                    columns: &columns,
                    fks: &[],
                };
                let _ = suggest(s, c, cat);
            }
        });
    }
}
