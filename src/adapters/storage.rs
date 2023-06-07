pub mod sled_db;

use async_trait::async_trait;
use fuels::tx::Bytes32;
use serde::{Deserialize, Serialize};

use crate::{common::EthTxStatus, errors::Result};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BlockSubmission {
    pub fuel_block_hash: Bytes32,
    pub fuel_block_height: u32,
    pub completed: bool,
    pub submitted_at_height: ethers::types::U64
}

#[async_trait]
pub trait Storage: Send + Sync {
    async fn insert(&self, submission: BlockSubmission) -> Result<()>;
    async fn update(&self, submission: BlockSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    async fn set_submission_completed(&self, fuel_block_hash: Bytes32) -> Result<()>;
}
