pub mod sqlite_db;

use async_trait::async_trait;
use ethers::types::H256;

use crate::{common::EthTxStatus, errors::Result};

#[derive(Debug, Clone, PartialEq, Eq)]
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
}
