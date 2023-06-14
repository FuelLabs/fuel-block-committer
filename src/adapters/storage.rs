pub mod sqlite_db;

use async_trait::async_trait;
use fuels::tx::Bytes32;
use serde::{Deserialize, Serialize};

use crate::errors::Result;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BlockSubmission {
    pub fuel_block_hash: Bytes32,
    pub fuel_block_height: u32,
    pub completed: bool,
    pub submitted_at_height: ethers::types::U64,
}

impl BlockSubmission {
    #[cfg(test)]
    pub fn random() -> Self {
        use rand::Rng;

        let mut rand = rand::thread_rng();
        Self {
            fuel_block_hash: rand.gen::<[u8; 32]>().into(),
            fuel_block_height: rand.gen(),
            completed: false,
            submitted_at_height: rand.gen::<u64>().into(),
        }
    }
}

#[async_trait]
pub trait Storage: Send + Sync {
    async fn insert(&self, submission: BlockSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    async fn set_submission_completed(&self, fuel_block_hash: Bytes32) -> Result<BlockSubmission>;
}
