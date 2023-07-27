mod fuel_client;
mod fuel_metrics;

pub use fuel_client::FuelClient;
use serde::{Deserialize, Serialize};

use crate::errors::Result;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct FuelBlock {
    pub hash: [u8; 32],
    pub height: u32,
}

impl std::fmt::Debug for FuelBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hash = self.hash.map(|byte| format!("{byte:02x?}")).join("");
        f.debug_struct("FuelBlock")
            .field("hash", &hash)
            .field("height", &self.height)
            .finish()
    }
}

#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait FuelAdapter: Send + Sync {
    async fn block_at_height(&self, height: u32) -> Result<Option<FuelBlock>>;
    async fn latest_block(&self) -> Result<FuelBlock>;
}
