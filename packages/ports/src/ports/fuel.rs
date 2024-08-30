pub use fuel_core_client::client::types::{
    block::{
        Block as FuelBlock, Consensus as FuelConsensus, Header as FuelHeader,
        PoAConsensus as FuelPoAConsensus,
    },
    primitives::{BlockId as FuelBlockId, Bytes32 as FuelBytes32, PublicKey as FuelPublicKey},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait Api: Send + Sync {
    async fn block_at_height(&self, height: u32) -> Result<Option<FuelBlock>>;
    async fn latest_block(&self) -> Result<FuelBlock>;
}
