#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{0}")]
    Postgres(#[from] postgres::Error),
    #[error("{0}")]
    MySql(#[from] mysql::Error),
    #[error("{0}")]
    Other(String),
}

pub type DbResult<T> = Result<T, DbError>;
