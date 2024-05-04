#![deny(unused_crate_dependencies)]
use std::pin::Pin;

use async_trait::async_trait;
use ethers::{
    prelude::{ContractError, SignerMiddleware},
    providers::{Provider, Ws},
    signers::LocalWallet,
    types::U256,
};
use futures::{stream::TryStreamExt, Stream};
use ports::{eth_rpc::FuelBlockCommittedOnEth, EthHeight};
use websocket::EthEventStreamer;

mod metrics;
mod websocket;

pub use ethers::types::{Address, Chain};
pub use websocket::WsAdapter;

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

type ContractErrorType =
    ethers::contract::ContractError<SignerMiddleware<Provider<Ws>, LocalWallet>>;
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

impl From<Error> for ports::eth_rpc::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::Network(err) => Self::Network(err),
            Error::Other(err) => Self::Other(err),
            Error::Wallet(err) => Self::Other(err.to_string()),
        }
    }
}

#[async_trait]
impl ports::eth_rpc::EthereumAdapter for WsAdapter {
    async fn submit(&self, block: ports::FuelBlock) -> ports::eth_rpc::Result<()> {
        Ok(self.submit(block).await?)
    }

    async fn get_block_number(&self) -> ports::eth_rpc::Result<ports::EthHeight> {
        let block_num = self.get_block_number().await?;
        let height = EthHeight::try_from(block_num)?;
        Ok(height)
    }

    fn event_streamer(
        &self,
        eth_block_height: u64,
    ) -> Box<dyn ports::eth_rpc::EventStreamer + Send + Sync> {
        let stream = self.event_streamer(eth_block_height);
        Box::new(stream)
    }

    async fn balance(&self) -> ports::eth_rpc::Result<U256> {
        Ok(self.balance().await?)
    }
}

#[async_trait::async_trait]
impl ports::eth_rpc::EventStreamer for EthEventStreamer {
    async fn establish_stream(
        &self,
    ) -> ports::eth_rpc::Result<
        Pin<Box<dyn Stream<Item = ports::eth_rpc::Result<FuelBlockCommittedOnEth>> + '_ + Send>>,
    > {
        let stream = self.establish_stream().await?.map_err(Into::into);
        Ok(Box::pin(stream))
    }
}
