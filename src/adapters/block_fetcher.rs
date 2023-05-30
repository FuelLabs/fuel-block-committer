pub mod fake_block_fetcher;
mod fuel_block_fetcher;
mod fuel_metrics;

pub use fuel_block_fetcher::FuelBlockFetcher;
use fuels::types::block::Block as FuelBlock;

use crate::errors::Result;

#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait BlockFetcher: Send + Sync {
    async fn latest_block(&self) -> Result<FuelBlock>;
}
