use std::{num::NonZeroUsize, pin::Pin};

use crate::types::{
    Fragment, FuelBlockCommittedOnL1, InvalidL1Height, L1Height, NonEmpty, Stream,
    TransactionResponse, U256,
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

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Contract: Sync {
    async fn submit(&self, hash: [u8; 32], height: u32) -> Result<()>;
    fn event_streamer(&self, height: L1Height) -> Box<dyn EventStreamer + Send + Sync>;
    fn commit_interval(&self) -> std::num::NonZeroU32;
}

#[derive(Debug, Clone, Copy)]
pub struct FragmentsSubmitted {
    pub tx: [u8; 32],
    pub num_fragments: NonZeroUsize,
}

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Api {
    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
    ) -> Result<FragmentsSubmitted>;
    async fn get_block_number(&self) -> Result<L1Height>;
    async fn balance(&self, address: crate::types::Address) -> Result<U256>;
    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>>;
}

pub trait FragmentEncoder {
    fn encode(&self, data: NonEmpty<u8>) -> Result<NonEmpty<Fragment>>;
    fn gas_usage(&self, num_bytes: NonZeroUsize) -> u64;
}

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait EventStreamer {
    async fn establish_stream<'a>(
        &'a self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<FuelBlockCommittedOnL1>> + 'a + Send>>>;
}
