mod fuel_block_fetcher;
mod health_tracker;
mod metrics;

pub use fuel_block_fetcher::FuelBlockFetcher;
use fuels::types::block::Block as FuelBlock;

use crate::errors::Result;

#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait BlockFetcher {
    async fn latest_block(&self) -> Result<FuelBlock>;
}
