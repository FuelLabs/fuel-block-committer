pub mod sled_db;

use async_trait::async_trait;
use ethers::types::H256;
use serde::{Deserialize, Serialize};

use crate::{common::EthTxStatus, errors::Result};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EthTxSubmission {
    pub fuel_block_height: u32,
    pub status: EthTxStatus,
    pub tx_hash: H256,
}

#[async_trait]
pub trait Storage: Send + Sync {
    async fn insert(&self, submission: EthTxSubmission) -> Result<()>;
    async fn update(&self, submission: EthTxSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<EthTxSubmission>>;
    async fn set_submission_status(&self, height: u32, status: EthTxStatus) -> Result<()>;
}
