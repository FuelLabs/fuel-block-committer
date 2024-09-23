#![deny(unused_crate_dependencies)]

use std::num::NonZeroU32;

use alloy::primitives::U256;
use async_trait::async_trait;
use ports::{
    l1::{Api, Contract, Result},
    types::{BlockSubmissionTx, L1Height, TransactionResponse, ValidatedFuelBlock},
};

mod aws;
mod error;
mod metrics;
mod websocket;

pub use alloy::primitives::Address;
pub use aws::*;
pub use websocket::WebsocketClient;

#[async_trait]
impl Contract for WebsocketClient {
    async fn submit(&self, block: ValidatedFuelBlock) -> Result<BlockSubmissionTx> {
        self.submit(block).await
    }

    fn commit_interval(&self) -> NonZeroU32 {
        self.commit_interval()
    }
}

#[async_trait]
impl Api for WebsocketClient {
    async fn submit_l2_state(&self, state_data: Vec<u8>) -> Result<[u8; 32]> {
        Ok(self.submit_l2_state(state_data).await?)
    }

    async fn balance(&self) -> Result<U256> {
        Ok(self.balance().await?)
    }

    async fn get_block_number(&self) -> Result<L1Height> {
        let block_num = self.get_block_number().await?;
        let height = L1Height::try_from(block_num)?;

        Ok(height)
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>> {
        Ok(self.get_transaction_response(tx_hash).await?)
    }
}
