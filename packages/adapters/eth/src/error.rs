use alloy::{
    signers::aws::AwsSignerError,
    transports::{RpcError, TransportErrorKind},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Network(String),
    #[error("network error: {0}")]
    TxExecution(String),
    #[error("other error: {0}")]
    Other(String),
}

impl From<RpcError<TransportErrorKind>> for Error {
    fn from(err: RpcError<TransportErrorKind>) -> Self {
        match err {
            RpcError::ErrorResp(err) if err.code >= -32613 && err.code <= -32000 => {
                Self::TxExecution(err.message)
            }
            _ => Self::Network(err.to_string()),
        }
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

impl From<Error> for services::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::Network(err) => Self::Network(err),
            Error::Other(err) | Error::TxExecution(err) => Self::Other(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy::rpc::json_rpc::ErrorPayload;

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
            let Error::Network(msg) = our_error else {
                panic!("Expected Network got: {}", our_error)
            };

            assert!(msg.contains("some message"));
        }
    }
}
