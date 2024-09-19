use std::{num::NonZeroUsize, pin::Pin, sync::Arc};

use crate::types::{
    FuelBlockCommittedOnL1, InvalidL1Height, L1Height, NonEmptyVec, Stream, TransactionResponse,
    ValidatedFuelBlock, U256,
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
    async fn submit(&self, block: ValidatedFuelBlock) -> Result<()>;
    fn event_streamer(&self, height: L1Height) -> Box<dyn EventStreamer + Send + Sync>;
    fn commit_interval(&self) -> std::num::NonZeroU32;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GasUsage {
    pub storage: u64,
    pub normal: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GasPrices {
    pub storage: u128,
    pub normal: u128,
}

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait Api {
    async fn gas_prices(&self) -> Result<GasPrices>;
    async fn submit_l2_state(&self, state_data: NonEmptyVec<u8>) -> Result<[u8; 32]>;
    async fn get_block_number(&self) -> Result<L1Height>;
    async fn balance(&self) -> Result<U256>;
    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>>;
}

#[async_trait::async_trait]
pub trait StorageCostCalculator {
    fn max_bytes_per_submission(&self) -> NonZeroUsize;
    fn gas_usage_to_store_data(&self, num_bytes: NonZeroUsize) -> GasUsage;
}
//
// #[async_trait::async_trait]
// impl<T: Api + Send + Sync> Api for Arc<T> {
//     fn max_bytes_per_submission(&self) -> NonZeroUsize {
//         (**self).max_bytes_per_submission()
//     }
//     fn gas_usage_to_store_data(&self, num_bytes: NonZeroUsize) -> GasUsage {
//         (**self).gas_usage_to_store_data(num_bytes)
//     }
//
//     async fn gas_prices(&self) -> Result<GasPrices> {
//         (**self).gas_prices().await
//     }
//
//     async fn submit_l2_state(&self, state_data: NonEmptyVec<u8>) -> Result<[u8; 32]> {
//         (**self).submit_l2_state(state_data).await
//     }
//     async fn get_block_number(&self) -> Result<L1Height> {
//         (**self).get_block_number().await
//     }
//     async fn balance(&self) -> Result<U256> {
//         (**self).balance().await
//     }
//     async fn get_transaction_response(
//         &self,
//         tx_hash: [u8; 32],
//     ) -> Result<Option<TransactionResponse>> {
//         (**self).get_transaction_response(tx_hash).await
//     }
// }

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait EventStreamer {
    async fn establish_stream<'a>(
        &'a self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<FuelBlockCommittedOnL1>> + 'a + Send>>>;
}
