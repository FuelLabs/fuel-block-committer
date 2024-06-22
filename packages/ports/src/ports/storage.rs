use std::sync::Arc;

use crate::types::{BlockSubmission, StateFragment, StateSubmission};

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
    // block submission
    async fn insert(&self, submission: BlockSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission>;

    // state submission
    async fn insert_state(
        &self,
        state: StateSubmission,
        fragments: Vec<StateFragment>,
    ) -> Result<()>;
    async fn get_unsubmitted_fragments(&self) -> Result<Vec<StateFragment>>;
}
