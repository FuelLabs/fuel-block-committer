mod monitored_adapter;
mod websocket;

use std::pin::Pin;

use async_trait::async_trait;
use ethers::types::{H160, U256};
use futures::Stream;
pub use monitored_adapter::MonitoredEthAdapter;
pub use websocket::EthereumWs;

use crate::{adapters::fuel_adapter::FuelBlock, errors::Result};

#[derive(Clone, Copy)]
pub struct FuelBlockCommitedOnEth {
    pub fuel_block_hash: [u8; 32],
    pub commit_height: U256,
}

impl std::fmt::Debug for FuelBlockCommitedOnEth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FuelBlockCommitedOnEth {{ hash: 0x")?;

        for h in &self.fuel_block_hash {
            write!(f, "{:x}", h)?;
        }

        write!(f, ", commit_height: {}}}", &self.commit_height)
    }
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
    async fn get_block_number(&self) -> Result<u64>;
    async fn balance(&self, address: H160) -> Result<U256>;
    fn event_streamer(&self, eth_block_height: u64) -> Box<dyn EventStreamer + Send + Sync>;
}
