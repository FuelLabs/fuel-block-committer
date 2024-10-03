use std::num::NonZeroU32;

use alloy::primitives::U256;
use delegate::delegate;
use ports::{
    l1::{Api, Contract, FragmentsSubmitted, Result},
    types::{BlockSubmissionTx, Fragment, L1Height, L1Tx, NonEmpty, TransactionResponse},
};

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
            async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx>;
            fn commit_interval(&self) -> NonZeroU32;
        }
    }
}

mod blob_encoding;
pub use blob_encoding::Eip4844BlobEncoder;

impl Api for WebsocketClient {
    delegate! {
        to (*self) {
            async fn submit_state_fragments(
                &self,
                fragments: NonEmpty<Fragment>,
                previous_tx: Option<ports::types::L1Tx>,
            ) -> Result<(L1Tx, FragmentsSubmitted)>;
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
