#![deny(unused_crate_dependencies)]
use std::pin::Pin;

use async_trait::async_trait;
use ethers::types::U256;
use futures::{stream::TryStreamExt, Stream};
use ports::types::{FuelBlockCommittedOnL1, L1Height};
use websocket::EthEventStreamer;

mod error;
mod metrics;
mod websocket;

pub use ethers::types::{Address, Chain};
pub use websocket::WebsocketClient;

#[async_trait]
impl ports::l1::Contract for WebsocketClient {
    async fn submit(&self, block: ports::types::FuelBlock) -> ports::l1::Result<()> {
        self.submit(block).await
    }

    async fn get_block_number(&self) -> ports::l1::Result<ports::types::L1Height> {
        let block_num = self.get_block_number().await?;
        let height = L1Height::try_from(block_num)?;
        Ok(height)
    }

    fn event_streamer(
        &self,
        eth_block_height: u64,
    ) -> Box<dyn ports::l1::EventStreamer + Send + Sync> {
        let stream = self.event_streamer(eth_block_height);
        Box::new(stream)
    }
}

#[async_trait]
impl ports::l1::Api for WebsocketClient {
    async fn balance(&self) -> ports::l1::Result<U256> {
        Ok(self.balance().await?)
    }
}

#[async_trait::async_trait]
impl ports::l1::EventStreamer for EthEventStreamer {
    async fn establish_stream(
        &self,
    ) -> ports::l1::Result<
        Pin<Box<dyn Stream<Item = ports::l1::Result<FuelBlockCommittedOnL1>> + '_ + Send>>,
    > {
        let stream = self.establish_stream().await?.map_err(Into::into);
        Ok(Box::pin(stream))
    }
}
