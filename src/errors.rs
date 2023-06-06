use actix_web::ResponseError;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Other(String),
    #[error("Network Error: {0}")]
    NetworkError(String),
    #[error("Storage Error: {0}")]
    StorageError(String),
}

impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        Self::StorageError(value.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::StorageError(value.to_string())
    }
}

impl ResponseError for Error {}

pub type Result<T> = std::result::Result<T, Error>;
