use async_trait::async_trait;
use fuels::types::block::Block;

use crate::errors::Result;

#[async_trait]
pub trait BlockFetcher {
    async fn latest_block(&self) -> Result<Block>;
}

pub struct FakeBlockFetcher {}

#[async_trait]
impl BlockFetcher for FakeBlockFetcher {
    async fn latest_block(&self) -> Result<Block> {
        todo!()
    }
}
