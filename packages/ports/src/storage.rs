use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
#[error("{msg}")]
pub struct Error {
    msg: String,
}

impl Error {
    pub fn new(msg: String) -> Self {
        Self { msg }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[async_trait::async_trait]
#[impl_tools::autoimpl(for<T: trait> &T, &mut T, Arc<T>, Box<T>)]
pub trait Storage: Send + Sync {
    async fn insert(&self, submission: crate::BlockSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<crate::BlockSubmission>>;
    async fn set_submission_completed(
        &self,
        fuel_block_hash: [u8; 32],
    ) -> Result<crate::BlockSubmission>;
}
