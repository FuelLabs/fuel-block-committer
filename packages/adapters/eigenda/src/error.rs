#[derive(Debug, thiserror::Error)]
pub enum EigenDAError {
    #[error("transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("RPC error: {0}")]
    Rpc(#[from] tonic::Status),
    #[error("authentication failed")]
    AuthenticationFailed,
    #[error("other error: {0}")]
    Other(String),
}
