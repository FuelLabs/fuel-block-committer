use std::ops::{Range, RangeInclusive};

pub use fuel_core_client::client::types::{
    block::{
        Block as FuelBlock, Consensus as FuelConsensus, Header as FuelHeader,
        PoAConsensus as FuelPoAConsensus,
    },
    primitives::{BlockId as FuelBlockId, Bytes32 as FuelBytes32, PublicKey as FuelPublicKey},
};
pub use futures::stream::BoxStream;

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
#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait Api: Send + Sync {
    async fn block_at_height(&self, height: u32) -> Result<Option<FuelBlock>>;
    fn blocks_in_height_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> BoxStream<Result<FuelBlock>, '_>;
    async fn latest_block(&self) -> Result<FuelBlock>;
}
