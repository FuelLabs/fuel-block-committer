pub mod eth_event_streamer;
pub mod ethereum_rpc;

use std::pin::Pin;

use async_trait::async_trait;
use ethers::types::U64;
use fuels::{tx::Bytes32, types::block::Block};
use futures::Stream;

use crate::errors::Result;

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait EventStreamer {
    async fn establish_stream<'a>(
        &'a self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes32>> + 'a + Send>>>;
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait EthereumAdapter: Send + Sync {
    async fn submit(&self, block: Block) -> Result<()>;
    async fn get_latest_eth_block(&self) -> Result<U64>;
    fn event_streamer(&self, eth_block_height: u64) -> Box<dyn EventStreamer + Send + Sync>;
}
