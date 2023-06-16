mod monitored_adapter;
mod websocket;

pub use monitored_adapter::MonitoredEthAdapter;
pub use websocket::EthereumWs;

use std::pin::Pin;

use async_trait::async_trait;
use ethers::types::{U256, U64};
use fuels::{tx::Bytes32, types::block::Block as FuelBlock};
use futures::Stream;

use crate::errors::Result;

#[derive(Debug, Clone, Copy)]
pub struct FuelBlockCommitedOnEth {
    pub fuel_block_hash: Bytes32,
    pub commit_height: U256,
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait EventStreamer {
    async fn establish_stream<'a>(
        &'a self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<FuelBlockCommitedOnEth>> + 'a + Send>>>;
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait EthereumAdapter: Send + Sync {
    async fn submit(&self, block: FuelBlock) -> Result<()>;
    async fn get_latest_eth_block(&self) -> Result<U64>;
    fn event_streamer(&self, eth_block_height: u64) -> Box<dyn EventStreamer + Send + Sync>;
}
