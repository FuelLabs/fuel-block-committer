pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Database Error {0}")]
    Database(String),
    #[error("Could not convert to/from domain/db type {0}")]
    Conversion(String),
}

impl From<Error> for ports::storage::Error {
    fn from(value: Error) -> Self {
        match value {
            Error::Database(e) => Self::Database(e),
            Error::Conversion(e) => Self::Conversion(e),
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Self::Database(e.to_string())
    }
}

impl From<sqlx::migrate::MigrateError> for Error {
    fn from(e: sqlx::migrate::MigrateError) -> Self {
        Self::Database(e.to_string())
    }
}
