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

impl Error {
    pub fn add_context(&mut self, ctx: &str) -> &mut Self {
        match self {
            Self::Other(msg) | Self::Network(msg) | Self::Storage(msg) => {
                *msg = format!("{}:\n{}", ctx, msg);
            }
        }
        self
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

impl From<crate::adapters::storage::Error> for Error {
    fn from(error: crate::adapters::storage::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<config::ConfigError> for Error {
    fn from(error: config::ConfigError) -> Self {
        Self::Storage(error.to_string())
    }
}

impl ResponseError for Error {}

pub type Result<T> = std::result::Result<T, Error>;
