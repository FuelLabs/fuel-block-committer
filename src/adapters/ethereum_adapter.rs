pub mod block_committed_event_streamer;
pub mod ethereum_rpc;

use async_trait::async_trait;
use ethers::types::U64;
use fuels::types::block::Block;

use crate::{
    adapters::ethereum_adapter::block_committed_event_streamer::BlockCommittedEventStreamer,
    errors::Result,
};

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait EthereumAdapter: Send + Sync {
    async fn submit(&self, block: Block) -> Result<()>;
    async fn get_latest_eth_block(&self) -> Result<U64>;
    fn commit_streamer(&self, eth_block_height: u64) -> Result<BlockCommittedEventStreamer>;
}
