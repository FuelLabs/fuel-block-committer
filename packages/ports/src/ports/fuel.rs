use std::ops::RangeInclusive;

pub use fuel_core_client::client::types::{
    block::{
        Block as FuelBlock, Consensus as FuelConsensus, Genesis, Genesis as FuelGenesis,
        Header as FuelHeader, PoAConsensus as FuelPoAConsensus,
    },
    primitives::{BlockId as FuelBlockId, Bytes32 as FuelBytes32, PublicKey as FuelPublicKey},
    Consensus,
};

#[derive(Debug, Clone)]
pub struct FullFuelBlock {
    pub id: FuelBytes32,
    pub header: FuelHeader,
    pub consensus: Consensus,
    pub raw_transactions: Vec<NonEmpty<u8>>,
}

#[derive(Debug, Clone)]
pub struct CompressedFuelBlock {
    pub height: u32,
    pub data: NonEmpty<u8>,
}

pub use futures::stream::BoxStream;

use crate::types::NonEmpty;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Api: Send + Sync {
    async fn block_at_height(&self, height: u32) -> Result<Option<FuelBlock>>;
    fn compressed_blocks_in_height_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> BoxStream<'_, Result<CompressedFuelBlock>>;
    async fn latest_block(&self) -> Result<FuelBlock>;
    async fn latest_height(&self) -> Result<u32>;
}
