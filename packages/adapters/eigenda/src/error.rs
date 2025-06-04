#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("RPC error: {0}")]
    Rpc(#[from] tonic::Status),
    #[error("authentication failed")]
    AuthenticationFailed,
    #[error("EigenDA client error: {0}")]
    EigenDAClient(#[from] rust_eigenda_v2_client::errors::EigenClientError),
    #[error("other error: {0}")]
    Other(String),
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
