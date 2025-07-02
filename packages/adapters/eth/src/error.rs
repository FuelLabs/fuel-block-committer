use alloy::{
    signers::aws::AwsSignerError,
    transports::{RpcError, TransportErrorKind},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {msg}, recoverable: {recoverable}")]
    Network { msg: String, recoverable: bool },
    #[error("network error: {0}")]
    TxExecution(String),
    #[error("other error: {0}")]
    Other(String),
}

impl From<RpcError<TransportErrorKind>> for Error {
    fn from(err: RpcError<TransportErrorKind>) -> Self {
        match err {
            RpcError::ErrorResp(err) if err.code >= -32613 && err.code <= -32000 => {
                Self::TxExecution(err.message.to_string())
            }
            RpcError::Transport(
                TransportErrorKind::BackendGone | TransportErrorKind::PubsubUnavailable,
            ) => Self::Network {
                msg: err.to_string(),
                recoverable: false,
            },
            _ => Self::Network {
                msg: err.to_string(),
                recoverable: true,
            },
        }
    }
}

impl From<alloy::contract::Error> for Error {
    fn from(value: alloy::contract::Error) -> Self {
        match value {
            alloy::contract::Error::TransportError(e) => e.into(),
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

impl From<Error> for services::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::Network { msg: err, .. } => Self::Network(err),
            Error::Other(err) | Error::TxExecution(err) => Self::Other(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy::rpc::json_rpc::ErrorPayload;
    use alloy::transports::{RpcError, TransportErrorKind};

    use super::*;

    #[test]
    fn correctly_detects_tx_execution_error() {
        for code in 32_000..=32613 {
            let err = RpcError::ErrorResp(ErrorPayload {
                code: -code,
                message: "some message".to_owned(),
                data: None,
            });

            let our_error = crate::error::Error::from(err);
            let Error::TxExecution(msg) = our_error else {
                panic!("Expected TxExecution got: {}", our_error)
            };

            assert!(msg.contains("some message"));
        }
    }

    #[test]
    fn rest_of_the_error_range_is_classified_as_network_caused() {
        for code in [31_999, 32614] {
            let err = RpcError::ErrorResp(ErrorPayload {
                code: -code,
                message: "some message".to_owned(),
                data: None,
            });

            let our_error = crate::error::Error::from(err);
            let Error::Network { msg, .. } = our_error else {
                panic!("Expected Network got: {}", our_error)
            };

            assert!(msg.contains("some message"));
        }
    }

    #[test]
    fn correctly_detects_irrecoverable_backend_gone_error() {
        let err = RpcError::Transport(TransportErrorKind::BackendGone);
        let our_error = crate::error::Error::from(err);
        match our_error {
            Error::Network { msg, recoverable } => {
                assert!(!recoverable, "BackendGone error should be irrecoverable");
                assert!(msg.contains("backend connection task has stopped"),);
            }
            _ => panic!(
                "Expected Network error for BackendGone, got: {:?}",
                our_error
            ),
        }
    }

    #[test]
    fn correctly_detects_irrecoverable_pubsub_unavailable_error() {
        let err = RpcError::Transport(TransportErrorKind::PubsubUnavailable);
        let our_error = crate::error::Error::from(err);
        match our_error {
            Error::Network { msg, recoverable } => {
                assert!(
                    !recoverable,
                    "PubsubUnavailable error should be irrecoverable"
                );
                assert!(
                    msg.contains("subscriptions are not available on this provider"),
                    "Error message should mention PubsubUnavailable"
                );
            }
            _ => panic!(
                "Expected Network error for PubsubUnavailable, got: {:?}",
                our_error
            ),
        }
    }
}
