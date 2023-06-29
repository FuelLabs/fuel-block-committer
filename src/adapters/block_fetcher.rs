mod fuel_block_fetcher;
mod fuel_metrics;

pub use fuel_block_fetcher::FuelBlockFetcher;
use serde::{Deserialize, Serialize};

use crate::errors::Result;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct FuelBlock {
    pub hash: [u8; 32],
    pub height: u32,
}

impl std::fmt::Debug for FuelBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FuelBlock {{ hash: 0x")?;

        for h in &self.hash {
            write!(f, "{:x}", h)?;
        }

        write!(f, ", height: {}}}", &self.height)
    }
}

#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait BlockFetcher: Send + Sync {
    async fn latest_block(&self) -> Result<FuelBlock>;
}
