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
    /// `:name`, filled in by the parameters prompt before running.
    Param,
}

/// Every `Kind`, in declaration order (indexed by `kind as usize`).
const KINDS: [Kind; 7] = [
    Kind::Keyword,
    Kind::Str,
    Kind::Number,
    Kind::Comment,
    Kind::Ident,
    Kind::Punct,
    Kind::Param,
];

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
        } else if c == b':'
            && (i == 0
                || !(b[i - 1] == b':'
                    || b[i - 1].is_ascii_alphanumeric()
                    || b[i - 1] == b'_'
                    || b[i - 1] == b']'))
            && b.get(i + 1)
                .is_some_and(|n| n.is_ascii_alphabetic() || *n == b'_' || *n >= 0x80)
        {
            // Same rule as `sql::param_spans`: not `::`, not `a[1:2]`, not `:=`.
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80) {
                i += 1;
            }
            Kind::Param
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
    if theme::is_light() {
        theme::oklch(0.50, 0.17, 290.0)
    } else {
        theme::oklch(0.80, 0.11, 290.0)
    }
}

fn color(kind: Kind, t: &Tokens) -> Color32 {
    match kind {
        Kind::Keyword => keyword_color(),
        Kind::Str => {
            if theme::is_light() {
                theme::oklch(0.52, 0.14, 145.0)
            } else {
                theme::oklch(0.80, 0.12, 145.0)
            }
        }
        Kind::Number => t.number,
        Kind::Comment => t.subtle,
        Kind::Ident => t.foreground,
        Kind::Punct => t.muted_foreground,
        Kind::Param => t.warning,
    }
}

/// For one-line fields (the WHERE filter): long text scrolls sideways instead of wrapping
/// into a column of lines.
pub fn sql_line_layouter(
    size: f32,
) -> impl FnMut(&egui::Ui, &dyn egui::TextBuffer, f32) -> std::sync::Arc<egui::Galley> {
    move |ui, buf, _wrap| {
        let mut job = highlight(buf.as_str(), &FontId::monospace(size));
        job.wrap.max_width = f32::INFINITY;
        ui.fonts_mut(|f| f.layout_job(job))
    }
}

pub fn highlight(text: &str, font: &FontId) -> LayoutJob {
    let t = theme::tokens();
    // Worked out once, not per token: a big script has hundreds of thousands of them, and the
    // keyword and string colors are conversions.
    let formats = KINDS.map(|kind| {
        let mut fmt = TextFormat::simple(font.clone(), color(kind, t));
        fmt.italics = kind == Kind::Comment;
        fmt
    });
    let mut job = LayoutJob {
        text: text.to_owned(),
        ..Default::default()
    };
    let tokens = tokens(text);
    job.sections.reserve(tokens.len());
    let mut at = 0;
    let mut last = None;
    for (kind, s) in tokens {
        let end = at + s.len();
        // Neighbors of one kind share a section: fewer of them to hash and lay out.
        match job.sections.last_mut() {
            Some(sec) if last == Some(kind) => sec.byte_range.end = egui::text::ByteIndex(end),
            _ => job.sections.push(egui::text::LayoutSection {
                leading_space: 0.0,
                byte_range: egui::text::ByteIndex(at)..egui::text::ByteIndex(end),
                format: formats[kind as usize].clone(),
            }),
        }
        last = Some(kind);
        at = end;
    }
    job
}

/// JSON in color: keys, strings, numbers, true/false/null, punctuation.
pub fn highlight_json(text: &str, font: &FontId) -> LayoutJob {
    let t = theme::tokens();
    let mut job = LayoutJob::default();
    let fmt = |c: Color32| TextFormat::simple(font.clone(), c);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < text.len() {
        let start = i;
        let c = bytes[i];
        let color = if c == b'"' {
            i += 1;
            while i < text.len() && bytes[i] != b'"' {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(text.len());
            // A key is a string followed by ':'.
            let key = text[i..].trim_start().starts_with(':');
            if key { t.info } else { color(Kind::Str, t) }
        } else if c == b'-' || c.is_ascii_digit() {
            while i < text.len() && matches!(bytes[i], b'0'..=b'9' | b'.' | b'-' | b'+' | b'e' | b'E') {
                i += 1;
            }
            t.number
        } else if c.is_ascii_alphabetic() {
            while i < text.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if &text[start..i] == "null" {
                t.subtle
            } else {
                t.boolean
            }
        } else {
            // Punctuation and whitespace, up to the next token (whole chars).
            i += text[i..].chars().next().map_or(1, char::len_utf8);
            while i < text.len() && !matches!(bytes[i], b'"' | b'-' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z')
            {
                i += text[i..].chars().next().map_or(1, char::len_utf8);
            }
            t.muted_foreground
        };
        job.append(&text[start..i], 0.0, fmt(color));
    }
    job
}

/// Underlines `range` (bytes) in red: where the last error is.
pub fn mark_error(job: &mut LayoutJob, range: std::ops::Range<usize>) {
    let t = super::theme::tokens();
    let mut out = Vec::with_capacity(job.sections.len() + 2);
    for sec in job.sections.drain(..) {
        let (start, end) = (sec.byte_range.start.0, sec.byte_range.end.0);
        let mut cuts = vec![start];
        cuts.extend(
            [range.start, range.end]
                .into_iter()
                .filter(|&p| p > start && p < end),
        );
        cuts.push(end);
        for (n, w) in cuts.windows(2).enumerate() {
            let mut piece = sec.clone();
            piece.byte_range = egui::text::ByteIndex(w[0])..egui::text::ByteIndex(w[1]);
            if n > 0 {
                piece.leading_space = 0.0;
            }
            if w[0] >= range.start && w[1] <= range.end {
                piece.format.underline = egui::Stroke::new(2.0, t.destructive);
                piece.format.background = super::theme::mix(t.destructive, 0.18, t.background);
            }
            out.push(piece);
        }
    }
    job.sections = out;
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
        let src = "SELECT é, 1.5 /* x */ FROM t";
        assert_eq!(tokens(src).iter().map(|(_, s)| *s).collect::<String>(), src);
    }

    #[test]
    fn highlight_covers_every_byte_once() {
        let src = "SELECT a, 'x' -- hi\n/* c */ FROM t WHERE id = :id;  ";
        let job = highlight(src, &FontId::monospace(12.0));
        assert_eq!(job.text, src);
        let mut at = 0;
        for (i, sec) in job.sections.iter().enumerate() {
            assert_eq!(
                sec.byte_range.start.0, at,
                "section {i} starts where the last ended"
            );
            at = sec.byte_range.end.0;
        }
        assert_eq!(at, src.len());
        assert!(
            job.sections.len() < tokens(src).len(),
            "neighbors of one kind are merged"
        );
        for (i, k) in KINDS.iter().enumerate() {
            assert_eq!(*k as usize, i);
        }
    }

    #[test]
    fn marks_parameters_but_not_casts() {
        let params: Vec<&str> = tokens("WHERE id = :id AND x::text = :name AND a[1:2]")
            .into_iter()
            .filter(|(k, _)| *k == Kind::Param)
            .map(|(_, s)| s)
            .collect();
        assert_eq!(params, [":id", ":name"]);
    }

    #[test]
    fn json_colors_cover_every_byte() {
        let text = "{\"name\": \"Zoë \\\"Zed\\\" Brontë\", \"v\": -1.5e3, \"ok\": true, \"x\": null, \"l\": [1, \"é\"]}";
        let job = highlight_json(text, &FontId::monospace(12.0));
        let covered: String = job
            .sections
            .iter()
            .map(|s| &text[s.byte_range.start.0..s.byte_range.end.0])
            .collect();
        assert_eq!(covered, text);
        // Unterminated strings (while typing) don't panic either.
        let _ = highlight_json("{\"a\": \"no end \\", &FontId::monospace(12.0));
    }
}
