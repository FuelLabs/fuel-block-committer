pub mod sqlite_db;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{adapters::block_fetcher::FuelBlock, errors::Result};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BlockSubmission {
    pub block: FuelBlock,
    pub completed: bool,
    // Eth block height moments before submitting the fuel block. Used to filter stale events in
    // the commit listener.
    pub submittal_height: u64,
}

impl BlockSubmission {
    #[cfg(test)]
    pub fn random() -> Self {
        use rand::Rng;

        let mut rand = rand::thread_rng();
        Self {
            block: FuelBlock {
                hash: rand.gen::<[u8; 32]>(),
                height: rand.gen(),
            },
            completed: false,
            submittal_height: rand.gen::<u64>(),
        }
    }
}

#[async_trait]
pub trait Storage: Send + Sync {
    async fn insert(&self, submission: BlockSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission>;
}
