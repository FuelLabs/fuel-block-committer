use std::fmt::Display;

use actix_web::ResponseError;
use tokio::task::JoinError;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Other(String),
    #[error("Network error: {0}")]
    Network(String),
    #[error("Storage error: {0}")]
    Storage(String),
}

pub trait WithContext<T> {
    fn with_context<C, F>(self, context: F) -> Result<T>
    where
        C: Display + Send + Sync + 'static,
        F: FnOnce() -> C;
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
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

impl From<services::ports::storage::Error> for Error {
    fn from(error: services::ports::storage::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<services::ports::l1::Error> for Error {
    fn from(error: services::ports::l1::Error) -> Self {
        match error {
            services::ports::l1::Error::Network(e) => Self::Network(e),
            services::ports::l1::Error::Other(e) => Self::Other(e),
        }
    }
}

impl From<services::ports::fuel::Error> for Error {
    fn from(error: services::ports::fuel::Error) -> Self {
        match error {
            services::ports::fuel::Error::Network(e) => Self::Network(e),
            services::ports::fuel::Error::Other(e) => Self::Other(e),
        }
    }
}

impl From<services::Error> for Error {
    fn from(error: services::Error) -> Self {
        match error {
            services::Error::Network(e) => Self::Network(e),
            services::Error::Storage(e) => Self::Storage(e),
            services::Error::BlockValidation(e) | services::Error::Other(e) => Self::Other(e),
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

impl<T> WithContext<T> for Result<T> {
    fn with_context<C, F>(self, context: F) -> Result<T>
    where
        C: Display + Send + Sync + 'static,
        F: FnOnce() -> C,
    {
        if let Err(err) = self {
            let new_err = match err {
                Error::Other(e) => Error::Other(format!("{}: {}", context(), e)),
                Error::Network(e) => Error::Network(format!("{}: {}", context(), e)),
                Error::Storage(e) => Error::Storage(format!("{}: {}", context(), e)),
            };
            Err(new_err)
        } else {
            self
        }
    }
}
