use std::{num::NonZeroU32, pin::Pin};

use alloy::primitives::U256;
use delegate::delegate;
use futures::{stream::TryStreamExt, Stream};
use ports::{
    l1::{Api, Contract, EventStreamer, FragmentsSubmitted, Result},
    types::{FuelBlockCommittedOnL1, L1Height, NonEmptyVec, TransactionResponse},
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
    delegate! {
        to self {
            async fn submit(&self, hash: [u8; 32], height: u32) -> Result<()>;
            fn commit_interval(&self) -> NonZeroU32;
        }
    }

    fn event_streamer(&self, height: L1Height) -> Box<dyn EventStreamer + Send + Sync> {
        Box::new(self.event_streamer(height.into()))
    }
}

mod blob_encoding;
pub use blob_encoding::Eip4844BlobEncoder;

impl Api for WebsocketClient {
    delegate! {
        to (*self) {
            async fn submit_state_fragments(
                &self,
                fragments: NonEmptyVec<NonEmptyVec<u8>>,
            ) -> Result<FragmentsSubmitted>;
            async fn balance(&self) -> Result<U256>;
            async fn get_transaction_response(&self, tx_hash: [u8; 32],) -> Result<Option<TransactionResponse>>;
        }
    }

    async fn get_block_number(&self) -> Result<L1Height> {
        let block_num = self._get_block_number().await?;
        let height = L1Height::try_from(block_num)?;

        Ok(height)
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
