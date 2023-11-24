use actix_web::ResponseError;
use ethers::signers::WalletError;
use tokio::task::JoinError;
use url::ParseError;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Other(String),
    #[error("Network Error: {0}")]
    Network(String),
    #[error("Storage Error: {0}")]
    Storage(String),
}

impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<WalletError> for Error {
    fn from(error: WalletError) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<ParseError> for Error {
    fn from(error: ParseError) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<JoinError> for Error {
    fn from(error: JoinError) -> Self {
        Self::Other(error.to_string())
    }
}

impl ResponseError for Error {}

pub type Result<T> = std::result::Result<T, Error>;
