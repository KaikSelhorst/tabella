//! Minimal SQL highlighter for the editor: keywords, strings, numbers, comments, quoted identifiers.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, FontId};

use super::theme::{self, Tokens};

pub const KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASE",
    "CAST",
    "COLUMN",
    "COMMIT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "DATABASE",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FETCH",
    "FOREIGN",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "ILIKE",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PRIMARY",
    "REFERENCES",
    "RETURNING",
    "RIGHT",
    "ROLLBACK",
    "SCHEMA",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "TRUE",
    "TRUNCATE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WITH",
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Keyword,
    Str,
    Number,
    Comment,
    Ident,
    Punct,
}

/// Splits `text` into colored spans. Every byte ends up in exactly one span.
fn tokens(text: &str) -> Vec<(Kind, &str)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let c = b[i];
        let kind = if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            Kind::Comment
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(b.len());
            Kind::Comment
        } else if c == b'\'' || c == b'"' || c == b'`' {
            i += 1;
            while i < b.len() && b[i] != c {
                i += 1;
            }
            i = (i + 1).min(b.len());
            if c == b'\'' { Kind::Str } else { Kind::Ident }
        } else if c.is_ascii_digit() {
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            Kind::Number
        } else if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80) {
                i += 1;
            }
            let word = &text[start..i];
            if KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(word)) {
                Kind::Keyword
            } else {
                Kind::Ident
            }
        } else {
            i += 1;
            Kind::Punct
        };
        out.push((kind, &text[start..i]));
    }
    out
}

pub fn keyword_color() -> Color32 {
    theme::oklch(0.80, 0.11, 290.0)
}

fn color(kind: Kind, t: &Tokens) -> Color32 {
    match kind {
        Kind::Keyword => keyword_color(),
        Kind::Str => theme::oklch(0.80, 0.12, 145.0),
        Kind::Number => t.number,
        Kind::Comment => t.subtle,
        Kind::Ident => t.foreground,
        Kind::Punct => t.muted_foreground,
    }
}

/// Layouter for an SQL `TextEdit` (`.layouter(&mut sql_layouter(13.0))`).
pub fn sql_layouter(
    size: f32,
) -> impl FnMut(&egui::Ui, &dyn egui::TextBuffer, f32) -> std::sync::Arc<egui::Galley> {
    move |ui, buf, wrap| {
        let mut job = highlight(buf.as_str(), FontId::monospace(size));
        job.wrap.max_width = wrap;
        ui.fonts_mut(|f| f.layout_job(job))
    }
}

pub fn highlight(text: &str, font: FontId) -> LayoutJob {
    let t = theme::tokens();
    let mut job = LayoutJob::default();
    for (kind, s) in tokens(text) {
        let mut fmt = TextFormat::simple(font.clone(), color(kind, t));
        fmt.italics = kind == Kind::Comment;
        job.append(s, 0.0, fmt);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sql() {
        let t = tokens("select 'a''b', 42 -- hi\nFROM \"T\"");
        let kinds: Vec<_> = t
            .iter()
            .filter(|(k, _)| *k != Kind::Punct)
            .map(|(k, s)| (*k, *s))
            .collect();
        assert_eq!(
            kinds,
            [
                (Kind::Keyword, "select"),
                (Kind::Str, "'a'"),
                (Kind::Str, "'b'"),
                (Kind::Number, "42"),
                (Kind::Comment, "-- hi"),
                (Kind::Keyword, "FROM"),
                (Kind::Ident, "\"T\""),
            ]
        );
        // Nothing is lost: spans concatenate back to the input.
        let src = "SELECT ç, 1.5 /* x */ FROM t";
        assert_eq!(tokens(src).iter().map(|(_, s)| *s).collect::<String>(), src);
    }
}
