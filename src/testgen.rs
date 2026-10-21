//! Text for robustness tests: random mixes of the pieces parsers trip on (open quotes, comment
//! and `$$` markers, casts, multi-byte and case-changing letters, line ends), from fixed seeds
//! so a failure repeats. The release build aborts on a panic, so none may come from what a user
//! types or pastes.

use std::panic::{AssertUnwindSafe, catch_unwind};

#[rustfmt::skip]
const PIECES: &[&str] = &[
    "SELECT", "select", "FROM", "WHERE", "UPDATE", "DELETE", "INSERT", "INTO", "WITH", "BEGIN",
    "COMMIT", "FOR", "SHARE", "VALUES", "TABLE", "AS", "JOIN", "ON", "COPY", "stdin", "DELIMITER",
    "t", "x", "id", "s.t", "\"T\"", "`b`", "1", "1.5", "-2e3", " ", "  ", "\t", "\n", "\r\n",
    "\n\n", ";", ",", ".", "(", ")", "[", "]", "{", "}", "'", "\"", "`", "\\", "$", "$$", "$a$",
    "--", "/*", "*/", ":", "::", ":p", ":=", "=", "*", "é", "ß", "İ", "Σ", "ﬃ", "😀", "\u{301}",
    "null", "true", "\"k\":", "near '", "' at line ", "at offset ", "no such column: ", "\\.",
    "postgres://", "mysql://", "sqlite:", "@", "?", "&", "%", "%zz", "/",
];

/// A xorshift generator: small, and the same sequence everywhere.
pub struct Gen(u64);

impl Gen {
    pub fn new(seed: u64) -> Self {
        Gen(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A number in `0..n` (`n` > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// Up to `max` pieces back to back.
    pub fn text_of(&mut self, max: usize) -> String {
        let n = self.below(max + 1);
        (0..n).map(|_| PIECES[self.below(PIECES.len())]).collect()
    }

    pub fn text(&mut self) -> String {
        self.text_of(40)
    }
}

/// Runs `f` on `n` random texts; a panic names the text that caused it.
pub fn each_text(seed: u64, n: usize, mut f: impl FnMut(&str, &mut Gen)) {
    let mut g = Gen::new(seed);
    // Short and empty texts first: the edges.
    let mut texts: Vec<String> = ["", " ", "\n", "'", "$$", "/*", "--", "é", "😀"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    texts.extend(PIECES.iter().map(|s| s.to_string()));
    for _ in 0..n {
        texts.push(g.text());
    }
    for text in texts {
        if let Err(e) = catch_unwind(AssertUnwindSafe(|| f(&text, &mut g))) {
            let why = e
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| e.downcast_ref::<&str>().copied())
                .unwrap_or("?");
            panic!("failed on {text:?}: {why}");
        }
    }
}

/// Whether `i` cuts `s` between chars (what slicing at it needs).
pub fn on_boundary(s: &str, i: usize) -> bool {
    i <= s.len() && s.is_char_boundary(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_texts() {
        let (mut a, mut b) = (Gen::new(7), Gen::new(7));
        let x: Vec<String> = (0..20).map(|_| a.text()).collect();
        assert_eq!(x, (0..20).map(|_| b.text()).collect::<Vec<_>>());
        assert!(
            x.iter().any(|t| t.chars().count() != t.len()),
            "multi-byte text comes up"
        );
    }
}
