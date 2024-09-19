use std::sync::Arc;

use crate::types::{
    BlockSubmission, BlockSubmissionTx, FuelBlockHeight, StateFragment, StateSubmission,
    SubmissionTx, TransactionState,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("db response: {0}")]
    Database(String),
    #[error("data conversion app<->db failed: {0}")]
    Conversion(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[async_trait::async_trait]
#[impl_tools::autoimpl(for<T: trait> &T, &mut T, Arc<T>, Box<T>)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Storage: Send + Sync {
    async fn record_block_submission(
        &self,
        submission_tx: BlockSubmissionTx,
        submission: BlockSubmission,
    ) -> Result<()>;
    async fn get_pending_block_submission_txs(&self) -> Result<Vec<BlockSubmissionTx>>;
    async fn update_block_submission_tx_state(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<FuelBlockHeight>;
    async fn transction_exists_for_block(&self, block_hash: [u8; 32]) -> Result<bool>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;

    async fn insert_state_submission(
        &self,
        submission: StateSubmission,
        fragments: Vec<StateFragment>,
    ) -> Result<()>;
    async fn get_unsubmitted_fragments(&self) -> Result<Vec<StateFragment>>;
    async fn record_state_submission(
        &self,
        tx_hash: [u8; 32],
        fragment_ids: Vec<u32>,
    ) -> Result<()>;
    async fn get_pending_txs(&self) -> Result<Vec<SubmissionTx>>;
    async fn has_pending_state_submission(&self) -> Result<bool>;
    async fn state_submission_w_latest_block(&self) -> Result<Option<StateSubmission>>;
    async fn update_submission_tx_state(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<()>;
}
