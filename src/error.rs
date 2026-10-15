#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{}", pg_message(.0))]
    Postgres(#[from] postgres::Error),
    #[error("{}", with_cert_hint(.0.to_string()))]
    MySql(#[from] mysql::Error),
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Other(String),
}

pub type DbResult<T> = Result<T, DbError>;

/// The server's own words. `postgres::Error`'s Display is only "db error" for anything the
/// server reports; the message, detail and hint are in the attached `DbError`.
fn pg_message(e: &postgres::Error) -> String {
    if let Some(db) = e.as_db_error() {
        let mut out = db.message().to_string();
        if let Some(d) = db.detail() {
            out.push_str(&format!(" — {d}"));
        }
        if let Some(h) = db.hint() {
            out.push_str(&format!(" (dica: {h})"));
        }
        return out;
    }
    // Connection problems: the cause (refused, TLS, auth...) is in the source chain.
    match std::error::Error::source(e) {
        Some(cause) => with_cert_hint(format!("{e}: {cause}")),
        None => e.to_string(),
    }
}

/// A certificate the client can't verify is usually a server with its own CA (AWS RDS,
/// self-signed): say which switch the user wants.
fn with_cert_hint(msg: String) -> String {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("certificate") || lower.contains("unknownissuer") {
        format!(
            "{msg} (o certificado do servidor não foi reconhecido; se ele usa uma CA própria, marque “Aceitar certificado inválido”)"
        )
    } else {
        msg
    }
}

impl DbError {
    /// Where in `sql` (byte offset) the server says the problem is: Postgres reports the
    /// position; MySQL names the text after it (`near '…' at line N`) or the unknown column or
    /// table, looked up in the statement.
    pub fn position_in(&self, sql: &str) -> Option<usize> {
        match self {
            DbError::Postgres(e) => match e.as_db_error()?.position()? {
                // 1-based, in characters.
                postgres::error::ErrorPosition::Original(n) => sql
                    .char_indices()
                    .nth((*n as usize).checked_sub(1)?)
                    .map(|(b, _)| b),
                postgres::error::ErrorPosition::Internal { .. } => None,
            },
            DbError::MySql(mysql::Error::MySqlError(e)) => mysql_position(&e.message, sql),
            // `near "FRM": syntax error`, `no such column: emial`, `no such table: x`
            DbError::Sqlite(e) => sqlite_position(&e.to_string(), sql),
            _ => None,
        }
    }
}

fn mysql_position(message: &str, sql: &str) -> Option<usize> {
    // "… for the right syntax to use near 'FRM clientes' at line 1"
    if let Some(at) = message.rfind("' at line ") {
        let line: usize = message[at + 10..].trim().parse().ok()?;
        let near = &message[message.find("near '")? + 6..at];
        let line_start = sql
            .split_inclusive('\n')
            .take(line.checked_sub(1)?)
            .map(str::len)
            .sum::<usize>();
        // MySQL cuts long "near" text: its first line is enough to find it.
        let needle = near.lines().next().unwrap_or("").trim();
        if needle.is_empty() {
            // Near '' means the statement ended too early.
            return Some(sql.trim_end().len().saturating_sub(1).min(sql.len()));
        }
        return Some(
            sql[line_start..]
                .find(needle)
                .map_or(line_start, |i| line_start + i),
        );
    }
    // "Unknown column 'x' in 'field list'", "Table 'loja.x' doesn't exist", "Unknown table 'x'"
    let name = message.split('\'').nth(1)?;
    let name = name.rsplit('.').next()?;
    find_word(sql, name)
}

fn sqlite_position(message: &str, sql: &str) -> Option<usize> {
    // Newer SQLite says exactly where: "… in SELECT … at offset 14".
    if let Some(at) = message
        .rsplit("at offset ")
        .next()
        .filter(|_| message.contains("at offset "))
        && let Ok(n) = at.trim().parse::<usize>()
        && n <= sql.len()
    {
        return Some(n);
    }
    if let Some(rest) = message.split("near \"").nth(1) {
        let near = rest.split('"').next()?;
        return find_word(sql, near).or_else(|| sql.find(near));
    }
    let name = message.rsplit(": ").next()?.trim();
    find_word(sql, name.rsplit('.').next()?)
}

/// First occurrence of `word` as a whole identifier, ignoring case.
fn find_word(sql: &str, word: &str) -> Option<usize> {
    if word.is_empty() {
        return None;
    }
    let (hay, needle) = (sql.to_lowercase(), word.to_lowercase());
    if hay.len() != sql.len() {
        return None;
    }
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(i) = hay[from..].find(&needle).map(|i| i + from) {
        let before = hay[..i].chars().next_back().is_none_or(|c| !ident(c));
        let after = hay[i + needle.len()..].chars().next().is_none_or(|c| !ident(c));
        if before && after {
            return Some(i);
        }
        from = i + needle.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_errors_point_at_the_statement() {
        let sql = "SELECT id\nFRM clientes\nWHERE id = 1";
        let msg = "You have an error in your SQL syntax; check the manual that corresponds to your MySQL \
                   server version for the right syntax to use near 'FRM clientes\nWHERE id = 1' at line 2";
        assert_eq!(mysql_position(msg, sql), Some(10));
        let sql = "SELECT nome, emial FROM clientes";
        assert_eq!(
            mysql_position("Unknown column 'emial' in 'field list'", sql),
            Some(13)
        );
        let sql = "select * from Produto";
        assert_eq!(
            mysql_position("Table 'loja.produto' doesn't exist", sql),
            Some(14)
        );
        // Not a word of its own: "id" inside "pid" doesn't count.
        assert_eq!(find_word("SELECT pid, id", "id"), Some(12));
        assert_eq!(mysql_position("Something else", "SELECT 1"), None);
    }
}
