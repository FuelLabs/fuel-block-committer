use actix_web::ResponseError;
use tokio::task::JoinError;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Other(String),
    #[error("Network Error: {0}")]
    Network(String),
    #[error("Storage error: {0}")]
    Storage(String),
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Other(value.to_string())
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

impl From<ports::storage::Error> for Error {
    fn from(error: ports::storage::Error) -> Self {
        Self::Storage(error.to_string())
    }
}
impl From<storage::Error> for Error {
    fn from(error: storage::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<ports::eth_rpc::Error> for Error {
    fn from(value: ports::eth_rpc::Error) -> Self {
        match value {
            ports::eth_rpc::Error::Network(e) => Self::Network(e),
            _ => Self::Other(value.to_string()),
        }
    }
}

impl From<eth_rpc::Error> for Error {
    fn from(value: eth_rpc::Error) -> Self {
        match value {
            eth_rpc::Error::Network(e) => Self::Network(e),
            _ => Self::Other(value.to_string()),
        }
    }
}

impl From<ports::fuel_rpc::Error> for Error {
    fn from(value: ports::fuel_rpc::Error) -> Self {
        match value {
            ports::fuel_rpc::Error::Network(e) => Self::Network(e),
        }
    }
}

impl From<config::ConfigError> for Error {
    fn from(error: config::ConfigError) -> Self {
        Self::Other(error.to_string())
    }
}

impl ResponseError for Error {}

pub type Result<T> = std::result::Result<T, Error>;
