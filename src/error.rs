#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{}", pg_message(.0))]
    Postgres(#[from] postgres::Error),
    #[error("{0}")]
    MySql(#[from] mysql::Error),
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
        Some(cause) => format!("{e}: {cause}"),
        None => e.to_string(),
    }
}
