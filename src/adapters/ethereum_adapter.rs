mod monitored_adapter;
mod websocket;

use std::pin::Pin;

use async_trait::async_trait;
use ethers::types::U256;
use futures::Stream;
pub use monitored_adapter::MonitoredEthAdapter;
pub use websocket::EthereumWs;

use crate::{adapters::block_fetcher::FuelBlock, errors::Result};

#[derive(Debug, Clone, Copy)]
pub struct FuelBlockCommitedOnEth {
    pub fuel_block_hash: [u8; 32],
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
    async fn get_latest_eth_block(&self) -> Result<u64>;
    fn event_streamer(&self, eth_block_height: u64) -> Box<dyn EventStreamer + Send + Sync>;
}
