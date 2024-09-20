use std::{num::NonZeroU32, pin::Pin};

use alloy::primitives::U256;
use futures::{stream::TryStreamExt, Stream};
use ports::{
    l1::{Api, Contract, EventStreamer, GasPrices, Result},
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

mod storage_gas_usage;
pub use storage_gas_usage::Eip4844GasUsage;

impl Api for WebsocketClient {
    async fn gas_prices(&self) -> Result<GasPrices> {
        self._gas_prices().await
    }

    async fn submit_l2_state(&self, state_data: NonEmptyVec<u8>) -> Result<[u8; 32]> {
        self._submit_l2_state(state_data).await
    }

    async fn balance(&self) -> Result<U256> {
        self._balance().await
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
        self._get_transaction_response(tx_hash).await
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
