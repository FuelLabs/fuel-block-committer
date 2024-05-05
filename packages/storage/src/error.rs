pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("Database Error {0}")]
    Database(String),
    #[error("Could not convert to/from domain/db type {0}")]
    Conversion(String),
}

impl From<Error> for ports::storage::Error {
    fn from(value: Error) -> ports::storage::Error {
        match value {
            Error::Database(e) => ports::storage::Error::Database(e),
            Error::Conversion(e) => ports::storage::Error::Conversion(e),
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
