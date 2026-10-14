//! Reading a big `.sql` file (a dump, a migration) one statement at a time, without loading it
//! whole: quotes, comments, Postgres `$tag$` bodies, MySQL `DELIMITER` lines and the data block
//! after pg_dump's `COPY … FROM stdin`.

use std::io::BufRead;

use crate::db::Kind;

#[derive(Debug, PartialEq)]
pub enum Item {
    /// A statement and the line it starts on.
    Stmt { sql: String, line: u64 },
    /// `COPY … FROM stdin` with the rows that follow it (up to `\.`).
    Copy { sql: String, data: Vec<u8>, line: u64 },
    /// A psql meta-command (`\connect`, `\set`…) the server wouldn't understand: skipped.
    Skipped { text: String, line: u64 },
}

pub struct ScriptReader<R: BufRead> {
    input: R,
    kind: Kind,
    /// Statement terminator; MySQL scripts change it with `DELIMITER`.
    delimiter: String,
    /// Bytes read so far (for the progress bar).
    pub bytes: u64,
    /// Lines read so far.
    pub line: u64,
    /// Statement being built, and where it started.
    buf: String,
    start: u64,
    /// Whether `buf` has anything besides comments and blanks.
    code: bool,
    state: State,
    /// Finished statements not handed out yet (a line can end several).
    ready: std::collections::VecDeque<Item>,
    done: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum State {
    Code,
    /// Inside quotes: the quote char, and whether backslash escapes (MySQL, Postgres E'').
    Quoted(char, bool),
    BlockComment,
    /// Inside `$tag$ … $tag$`.
    Dollar(String),
}

impl<R: BufRead> ScriptReader<R> {
    pub fn new(input: R, kind: Kind) -> Self {
        ScriptReader {
            input,
            kind,
            delimiter: ";".into(),
            bytes: 0,
            line: 0,
            buf: String::new(),
            start: 1,
            code: false,
            state: State::Code,
            ready: Default::default(),
            done: false,
        }
    }

    fn read_line(&mut self) -> std::io::Result<Option<String>> {
        let mut raw = Vec::new();
        let n = self.input.read_until(b'\n', &mut raw)?;
        if n == 0 {
            return Ok(None);
        }
        self.bytes += n as u64;
        self.line += 1;
        // Files saved by old Windows editors aren't UTF-8: read them as Windows-1252/Latin-1.
        Ok(Some(match String::from_utf8(raw) {
            Ok(s) => s,
            Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
        }))
    }

    fn finish_statement(&mut self) {
        let sql = std::mem::take(&mut self.buf).trim().to_string();
        // Comments alone aren't a statement (they'd be an empty query).
        if self.code && !sql.is_empty() {
            self.ready.push_back(Item::Stmt {
                sql,
                line: self.start,
            });
        }
        self.code = false;
    }

    /// Notes real code: the statement "starts" at its first line of code.
    fn mark_code(&mut self) {
        if !self.code {
            self.code = true;
            self.start = self.line;
        }
    }

    /// Feeds one line through the state machine.
    fn feed(&mut self, line: &str) {
        let at_start = !self.code && self.state == State::Code;
        let trimmed = line.trim();
        if at_start {
            // `DELIMITER $$` (mysql client syntax) changes the terminator.
            if self.kind == Kind::MySql
                && trimmed.len() > 10
                && trimmed[..10].eq_ignore_ascii_case("DELIMITER ")
            {
                self.delimiter = trimmed[10..].trim().to_string();
                return;
            }
            // psql meta-commands are for psql, not the server.
            if trimmed.starts_with('\\') {
                self.ready.push_back(Item::Skipped {
                    text: trimmed.to_string(),
                    line: self.line,
                });
                return;
            }
        }
        let chars: Vec<char> = line.chars().collect();
        let delim: Vec<char> = self.delimiter.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            match self.state.clone() {
                State::Code => {
                    if chars[i..].starts_with(&delim) {
                        i += delim.len();
                        self.finish_statement();
                        continue;
                    }
                    if c == '-' && chars.get(i + 1) == Some(&'-') || (c == '#' && self.kind == Kind::MySql) {
                        // Rest of the line is a comment (kept: it may carry hints).
                        self.buf.extend(&chars[i..]);
                        break;
                    }
                    if c == '/' && chars.get(i + 1) == Some(&'*') {
                        self.state = State::BlockComment;
                        self.buf.push_str("/*");
                        i += 2;
                        continue;
                    }
                    if !c.is_whitespace() {
                        self.mark_code();
                    }
                    if matches!(c, '\'' | '"' | '`') {
                        // MySQL escapes with backslash everywhere; Postgres only in E'...'.
                        let escapes = match self.kind {
                            Kind::MySql => c != '`',
                            Kind::Postgres => {
                                c == '\''
                                    && i > 0
                                    && matches!(chars[i - 1], 'E' | 'e')
                                    && (i < 2 || !chars[i - 2].is_alphanumeric())
                            }
                        };
                        self.state = State::Quoted(c, escapes);
                    } else if c == '$' && self.kind == Kind::Postgres {
                        let end =
                            (i + 1..chars.len()).find(|&j| !(chars[j].is_alphanumeric() || chars[j] == '_'));
                        if let Some(e) = end
                            && chars[e] == '$'
                            && !chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())
                        {
                            let tag: String = chars[i..=e].iter().collect();
                            self.buf.push_str(&tag);
                            self.state = State::Dollar(tag);
                            i = e + 1;
                            continue;
                        }
                    }
                    self.buf.push(c);
                }
                State::Quoted(q, escapes) => {
                    self.buf.push(c);
                    if escapes && c == '\\' {
                        if let Some(&n) = chars.get(i + 1) {
                            self.buf.push(n);
                            i += 1;
                        }
                    } else if c == q {
                        self.state = State::Code;
                    }
                }
                State::BlockComment => {
                    self.buf.push(c);
                    if c == '*' && chars.get(i + 1) == Some(&'/') {
                        self.buf.push('/');
                        i += 1;
                        self.state = State::Code;
                    }
                }
                State::Dollar(tag) => {
                    let t: Vec<char> = tag.chars().collect();
                    if chars[i..].starts_with(&t) {
                        self.buf.push_str(&tag);
                        i += t.len();
                        self.state = State::Code;
                        continue;
                    }
                    self.buf.push(c);
                }
            }
            i += 1;
        }
    }

    /// The rows after `COPY … FROM stdin;`, up to the `\.` line.
    fn copy_data(&mut self) -> std::io::Result<Vec<u8>> {
        let mut data = Vec::new();
        while let Some(line) = self.read_line()? {
            if line.trim_end_matches(['\r', '\n']) == "\\." {
                break;
            }
            data.extend_from_slice(line.as_bytes());
        }
        Ok(data)
    }
}

/// `COPY tabela (…) FROM stdin`: its data comes inline, right after it.
fn is_copy_from_stdin(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    let words: Vec<&str> = upper.split_whitespace().collect();
    words.first() == Some(&"COPY") && upper.contains("FROM STDIN")
}

impl<R: BufRead> Iterator for ScriptReader<R> {
    type Item = std::io::Result<Item>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.ready.pop_front() {
                if let Item::Stmt { sql, line } = &item
                    && self.kind == Kind::Postgres
                    && is_copy_from_stdin(sql)
                {
                    let (sql, line) = (sql.clone(), *line);
                    return Some(self.copy_data().map(|data| Item::Copy { sql, data, line }));
                }
                return Some(Ok(item));
            }
            if self.done {
                return None;
            }
            match self.read_line() {
                Ok(Some(line)) => self.feed(&line),
                Ok(None) => {
                    self.done = true;
                    // A last statement without its terminator still runs (an unclosed quote
                    // too: the server will say what's wrong with it).
                    self.finish_statement();
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(kind: Kind, text: &str) -> Vec<Item> {
        ScriptReader::new(std::io::Cursor::new(text.as_bytes()), kind)
            .map(Result::unwrap)
            .collect()
    }

    fn stmts(items: &[Item]) -> Vec<&str> {
        items
            .iter()
            .filter_map(|i| match i {
                Item::Stmt { sql, .. } => Some(sql.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn splits_like_a_database_client() {
        let pg = "SET x = 1;\nINSERT INTO t VALUES ('a;b', 'it''s');\n-- nada; aqui\nCREATE FUNCTION f() RETURNS int AS $$\nBEGIN\n  RETURN 1;\nEND;\n$$ LANGUAGE plpgsql;\nSELECT 'c:\\temp';\n/* um; dois */ SELECT 2\n";
        let items = read(Kind::Postgres, pg);
        assert_eq!(
            stmts(&items),
            [
                "SET x = 1",
                "INSERT INTO t VALUES ('a;b', 'it''s')",
                "-- nada; aqui\nCREATE FUNCTION f() RETURNS int AS $$\nBEGIN\n  RETURN 1;\nEND;\n$$ LANGUAGE plpgsql",
                // Standard strings: the backslash doesn't escape the closing quote.
                "SELECT 'c:\\temp'",
                "/* um; dois */ SELECT 2",
            ]
        );
        // Its first line of code, past the comment.
        assert!(matches!(items[2], Item::Stmt { line: 4, .. }), "{:?}", items[2]);
    }

    #[test]
    fn mysql_delimiters_and_escapes() {
        let my = "INSERT INTO t VALUES ('a\\';b');\n# comentário; ok\nDELIMITER ;;\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END ;;\nDELIMITER ;\nSELECT 3;\n";
        assert_eq!(
            stmts(&read(Kind::MySql, my)),
            [
                "INSERT INTO t VALUES ('a\\';b')",
                "# comentário; ok\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
                "SELECT 3",
            ]
        );
    }

    #[test]
    fn pg_dump_copy_blocks_and_meta_commands() {
        let dump = "\\connect loja\nCOPY public.t (id, nome) FROM stdin;\n1\tana\n2\tbia;\n\\.\nSELECT pg_catalog.setval('s', 2, true);\n";
        let items = read(Kind::Postgres, dump);
        assert_eq!(
            items[0],
            Item::Skipped {
                text: "\\connect loja".into(),
                line: 1
            }
        );
        assert_eq!(
            items[1],
            Item::Copy {
                sql: "COPY public.t (id, nome) FROM stdin".into(),
                data: b"1\tana\n2\tbia;\n".to_vec(),
                line: 2
            }
        );
        assert_eq!(stmts(&items), ["SELECT pg_catalog.setval('s', 2, true)"]);
    }

    #[test]
    fn counts_bytes_for_progress() {
        let text = "SELECT 1;\nSELECT 2;\n";
        let mut r = ScriptReader::new(std::io::Cursor::new(text.as_bytes()), Kind::Postgres);
        r.next();
        assert!(r.bytes > 0 && r.bytes <= text.len() as u64);
        while r.next().is_some() {}
        assert_eq!(r.bytes, text.len() as u64);
    }
}
