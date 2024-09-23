use alloy::{
    signers::aws::AwsSignerError,
    transports::{RpcError, TransportErrorKind},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Network(String),
    #[error("other error: {0}")]
    Other(String),
}

impl From<RpcError<TransportErrorKind>> for Error {
    fn from(err: RpcError<TransportErrorKind>) -> Self {
        Self::Network(err.to_string())
    }
}

impl From<alloy::contract::Error> for Error {
    fn from(value: alloy::contract::Error) -> Self {
        match value {
            alloy::contract::Error::TransportError(e) => Self::Network(e.to_string()),
            _ => Self::Other(value.to_string()),
        }
    }
}

impl From<alloy::sol_types::Error> for Error {
    fn from(value: alloy::sol_types::Error) -> Self {
        Self::Other(value.to_string())
    }
}

impl From<AwsSignerError> for Error {
    fn from(value: AwsSignerError) -> Self {
        Self::Other(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<Error> for ports::l1::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::Network(err) => Self::Network(err),
            Error::Other(err) => Self::Other(err),
        }
    }
}
