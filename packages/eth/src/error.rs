use alloy::transports::TransportErrorKind;
use ethers::{
    prelude::{ContractError, SignerMiddleware},
    providers::{Provider, Ws},
    signers::AwsSigner,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("wallet error: {0}")]
    Wallet(#[from] ethers::signers::WalletError),
    #[error("network error: {0}")]
    Network(String),
    #[error("other error: {0}")]
    Other(String),
}

impl From<ethers::providers::ProviderError> for Error {
    fn from(err: ethers::providers::ProviderError) -> Self {
        Self::Network(err.to_string())
    }
}

impl From<alloy::transports::RpcError<TransportErrorKind>> for Error {
    fn from(err: alloy::transports::RpcError<TransportErrorKind>) -> Self {
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

impl From<c_kzg::Error> for Error {
    fn from(value: c_kzg::Error) -> Self {
        Self::Other(value.to_string())
    }
}

impl From<alloy::sol_types::Error> for Error {
    fn from(value: alloy::sol_types::Error) -> Self {
        Self::Other(value.to_string())
    }
}

impl From<alloy::signers::aws::AwsSignerError> for Error {
    fn from(value: alloy::signers::aws::AwsSignerError) -> Self {
        Self::Other(value.to_string())
    }
}

pub type ContractErrorType =
    ethers::contract::ContractError<SignerMiddleware<Provider<Ws>, AwsSigner>>;

impl From<ContractErrorType> for Error {
    fn from(value: ContractErrorType) -> Self {
        match value {
            ContractError::MiddlewareError { e } => Self::Other(e.to_string()),
            ContractError::ProviderError { e } => Self::Network(e.to_string()),
            _ => Self::Other(value.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<Error> for ports::l1::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::Network(err) => Self::Network(err),
            Error::Other(err) => Self::Other(err),
            Error::Wallet(err) => Self::Other(err.to_string()),
        }
    }
}
