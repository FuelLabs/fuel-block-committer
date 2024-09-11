#![deny(unused_crate_dependencies)]

use std::{num::NonZeroU32, pin::Pin};

use alloy::primitives::U256;
use async_trait::async_trait;
use futures::{stream::TryStreamExt, Stream};
use ports::{
    l1::{Api, Contract, EventStreamer, Result},
    types::{
        FuelBlockCommittedOnL1, L1Height, NonEmptyVec, TransactionResponse, ValidatedFuelBlock,
    },
};
use websocket::EthEventStreamer;

mod aws;
mod error;
mod metrics;
mod websocket;

pub use alloy::primitives::Address;
pub use aws::*;
pub use websocket::WebsocketClient;

#[async_trait]
impl Contract for WebsocketClient {
    async fn submit(&self, block: ValidatedFuelBlock) -> Result<()> {
        self.submit(block).await
    }

    fn event_streamer(&self, height: L1Height) -> Box<dyn EventStreamer + Send + Sync> {
        Box::new(self.event_streamer(height.into()))
    }

    fn commit_interval(&self) -> NonZeroU32 {
        self.commit_interval()
    }
}

#[async_trait]
impl Api for WebsocketClient {
    fn split_into_submittable_state_chunks(
        &self,
        data: &NonEmptyVec<u8>,
    ) -> Result<NonEmptyVec<NonEmptyVec<u8>>> {
        Ok(self._split_into_submittable_state_chunks(data)?)
    }

    async fn submit_l2_state(&self, state_data: Vec<u8>) -> Result<[u8; 32]> {
        Ok(self._submit_l2_state(state_data).await?)
    }

    async fn balance(&self) -> Result<U256> {
        Ok(self._balance().await?)
    }

    async fn get_block_number(&self) -> Result<L1Height> {
        let block_num = self._get_block_number().await?;
        let height = L1Height::try_from(block_num)?;

        Ok(height)
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>> {
        Ok(self._get_transaction_response(tx_hash).await?)
    }
}

#[async_trait::async_trait]
impl EventStreamer for EthEventStreamer {
    async fn establish_stream(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<FuelBlockCommittedOnL1>> + '_ + Send>>> {
        let stream = self.establish_stream().await?.map_err(Into::into);

        Ok(Box::pin(stream))
    }
}
