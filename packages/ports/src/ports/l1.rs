use std::pin::Pin;

use crate::types::{
    BlockSubmissionTx, FuelBlockCommittedOnL1, InvalidL1Height, L1Height, Stream, TransactionResponse, ValidatedFuelBlock, U256
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Network(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<InvalidL1Height> for Error {
    fn from(err: InvalidL1Height) -> Self {
        Self::Other(err.to_string())
    }
}

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait Contract: Send + Sync {
    async fn submit(&self, block: ValidatedFuelBlock) -> Result<BlockSubmissionTx>;
    fn event_streamer(&self, height: L1Height) -> Box<dyn EventStreamer + Send + Sync>;
    fn commit_interval(&self) -> std::num::NonZeroU32;
}

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait Api {
    async fn submit_l2_state(&self, state_data: Vec<u8>) -> Result<[u8; 32]>;
    async fn get_block_number(&self) -> Result<L1Height>;
    async fn balance(&self) -> Result<U256>;
    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>>;
}

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait EventStreamer {
    async fn establish_stream<'a>(
        &'a self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<FuelBlockCommittedOnL1>> + 'a + Send>>>;
}
