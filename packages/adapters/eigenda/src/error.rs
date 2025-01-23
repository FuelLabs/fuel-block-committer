#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("Transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("RPC error: {0}")]
    Rpc(#[from] tonic::Status),
    #[error("Authentication failed")]
    AuthenticationFailed,
    #[error("Blob processing failed: {0}")]
    BlobProcessingFailed(String),
    #[error("Timeout waiting for blob status")]
    Timeout,
}
