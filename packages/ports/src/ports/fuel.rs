use std::ops::RangeInclusive;

pub use fuel_core_client::client::types::Consensus;
pub use fuel_core_client::client::types::{
    block::{
        Block as FuelBlock, Consensus as FuelConsensus, Genesis as FuelGenesis,
        Header as FuelHeader, PoAConsensus as FuelPoAConsensus,
    },
    primitives::{BlockId as FuelBlockId, Bytes32 as FuelBytes32, PublicKey as FuelPublicKey},
};

#[derive(Debug, Clone)]
pub struct FullFuelBlock {
    pub id: FuelBytes32,
    pub header: FuelHeader,
    pub consensus: Consensus,
    pub raw_transactions: Vec<NonEmptyVec<u8>>,
}

pub use futures::stream::BoxStream;

use crate::types::NonEmptyVec;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

// TODO: segfault
// https://github.com/FuelLabs/fuel-core-client-ext/blob/master/src/lib.rs
#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Api: Send + Sync {
    async fn block_at_height(&self, height: u32) -> Result<Option<FuelBlock>>;
    fn full_blocks_in_height_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> BoxStream<'_, Result<NonEmptyVec<FullFuelBlock>>>;
    async fn latest_block(&self) -> Result<FuelBlock>;
    async fn latest_height(&self) -> Result<u32>;
}
