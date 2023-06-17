mod fuel_block_fetcher;
mod fuel_metrics;

pub use fuel_block_fetcher::FuelBlockFetcher;
use serde::{Deserialize, Serialize};

use crate::errors::Result;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuelBlock {
    pub hash: [u8; 32],
    pub height: u32,
}

#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait BlockFetcher: Send + Sync {
    async fn latest_block(&self) -> Result<FuelBlock>;
}
