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
    async fn submit(&self, block: ports::types::ValidatedFuelBlock) -> ports::l1::Result<()> {
        self.submit(block).await
    }

    fn event_streamer(&self, height: L1Height) -> Box<dyn ports::l1::EventStreamer + Send + Sync> {
        let stream = self.event_streamer(height.into());
        Box::new(stream)
    }
}

#[async_trait]
impl ports::l1::Api for WebsocketClient {
    async fn balance(&self) -> ports::l1::Result<U256> {
        Ok(self.balance().await?)
    }

    async fn get_block_number(&self) -> ports::l1::Result<ports::types::L1Height> {
        let block_num = self.get_block_number().await?;
        let height = L1Height::try_from(block_num)?;
        Ok(height)
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
