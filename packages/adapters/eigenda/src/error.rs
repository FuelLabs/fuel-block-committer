use rust_eigenda_v2_client::errors::{ConversionError, PayloadDisperserError};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("RPC error: {0}")]
    Rpc(#[from] tonic::Status),
    #[error("authentication failed")]
    AuthenticationFailed,
    #[error("Invalid Ethereum RPC URL: {0}")]
    InvalidRPCUrl(#[from] url::ParseError),
    #[error("EigenDA client initialization failed: {0}")]
    EigenDAClientInit(anyhow::Error),
    #[error("EigenDA client error: {0}")]
    EigenDAClient(#[from] rust_eigenda_v2_client::errors::EigenClientError),
    #[error("Failed to dispatch blob: {0}")]
    BlockDispacthFailed(#[from] PayloadDisperserError),
    #[error("Invalid hex representation of blob key: {0}")]
    InvalidBlobKey(#[from] ConversionError),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<Error> for services::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::Transport(_) | Error::Rpc(_) => services::Error::Network(err.to_string()),
            Error::EigenDAClient(_) => services::Error::Network(err.to_string()),
            _ => services::Error::Other(err.to_string()),
        }
    }
}
