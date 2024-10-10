use std::ops::RangeInclusive;

use crate::types::NonEmpty;
pub use fuel_core_client::client::types::{
    block::{
        Block as FuelBlock, Consensus as FuelConsensus, Genesis, Genesis as FuelGenesis,
        Header as FuelHeader, PoAConsensus as FuelPoAConsensus,
    },
    primitives::{BlockId as FuelBlockId, Bytes32 as FuelBytes32, PublicKey as FuelPublicKey},
    Consensus,
};
pub use futures::stream::BoxStream;

#[derive(Debug, Clone)]
pub struct FullFuelBlock {
    pub id: FuelBytes32,
    pub header: FuelHeader,
    pub consensus: Consensus,
    pub raw_transactions: Vec<NonEmpty<u8>>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CompressedBlock {
    pub(crate) height: u32,
    pub(crate) hash: FuelBytes32,
    pub(crate) data: Vec<u8>,
}

impl CompressedBlock {
    pub fn new(height: u32, hash: FuelBytes32, data: Vec<u8>) -> Self {
        Self { height, hash, data }
    }
}

#[derive(Debug, Clone)]
pub enum MaybeCompressedFuelBlock {
    Compressed(CompressedBlock),
    Uncompressed(FullFuelBlock),
}

impl MaybeCompressedFuelBlock {
    pub fn height(&self) -> u32 {
        match self {
            Self::Compressed(block) => block.height,
            Self::Uncompressed(block) => block.header.height,
        }
    }

    pub fn into_uncompressed(self) -> Result<FullFuelBlock> {
        match self {
            Self::Compressed(_) => Err(Error::Other(
                "Cannot convert compressed block to uncompressed".to_string(),
            )),
            Self::Uncompressed(block) => Ok(block.clone()),
        }
    }
}

impl From<FullFuelBlock> for MaybeCompressedFuelBlock {
    fn from(block: FullFuelBlock) -> Self {
        Self::Uncompressed(block)
    }
}

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
    fn full_blocks_in_height_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> BoxStream<'_, Result<Vec<MaybeCompressedFuelBlock>>>;
    async fn latest_block(&self) -> Result<FuelBlock>;
    async fn latest_height(&self) -> Result<u32>;
}
