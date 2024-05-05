#![deny(unused_crate_dependencies)]
use std::pin::Pin;

use async_trait::async_trait;
use ethers::types::U256;
use futures::{stream::TryStreamExt, Stream};
use ports::types::{EthHeight, FuelBlockCommittedOnEth};
use websocket::EthEventStreamer;

mod error;
mod metrics;
mod websocket;

pub use ethers::types::{Address, Chain};
pub use websocket::WsAdapter;

#[async_trait]
impl ports::eth_rpc::EthereumAdapter for WsAdapter {
    async fn submit(&self, block: ports::types::FuelBlock) -> ports::eth_rpc::Result<()> {
        self.submit(block).await
    }

    async fn get_block_number(&self) -> ports::eth_rpc::Result<ports::types::EthHeight> {
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
