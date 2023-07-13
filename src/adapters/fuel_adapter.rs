mod fuel_client;
mod fuel_metrics;

pub use fuel_client::FuelClient;
use serde::{Deserialize, Serialize};

use crate::errors::Result;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuelBlock {
    pub hash: [u8; 32],
    pub height: u32,
}

#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait FuelAdapter: Send + Sync {
    async fn block_at_height(&self, height: u32) -> Result<Option<FuelBlock>>;
    async fn latest_block(&self) -> Result<FuelBlock>;
}
