use std::{pin::Pin, sync::Arc};

use futures::Stream;
use sqlx::types::chrono::{DateTime, Utc};

use crate::types::{
    BlockSubmission, StateFragment, StateSubmission, SubmissionTx, TransactionState,
    UnfinalizedSegmentData,
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
    async fn insert(&self, submission: BlockSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission>;

    async fn insert_state_submission(&self, submission: StateSubmission) -> Result<()>;
    fn stream_unfinalized_segment_data<'a>(
        &'a self,
    ) -> Pin<Box<dyn Stream<Item = Result<UnfinalizedSegmentData>> + 'a + Send>>;
    async fn record_pending_tx(&self, tx_hash: [u8; 32], fragment_ids: Vec<u32>) -> Result<()>;
    async fn get_pending_txs(&self) -> Result<Vec<SubmissionTx>>;
    async fn has_pending_txs(&self) -> Result<bool>;
    async fn state_submission_w_latest_block(&self) -> Result<Option<StateSubmission>>;
    async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>>;
    async fn update_submission_tx_state(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<()>;
}
