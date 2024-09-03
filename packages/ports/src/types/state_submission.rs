pub use sqlx::types::chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubmission {
    pub id: Option<u32>,
    pub block_hash: [u8; 32],
    pub block_height: u32,
}

#[derive(Clone, PartialEq, Eq)]
pub struct StateFragment {
    pub id: Option<u32>,
    pub submission_id: Option<u32>,
    pub fragment_idx: u32,
    pub data: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

impl std::fmt::Debug for StateFragment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateFragment")
            .field("id", &self.id)
            .field("submission_id", &self.submission_id)
            .field("fragment_idx", &self.fragment_idx)
            .field("data", &hex::encode(&self.data))
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl StateFragment {
    pub const MAX_FRAGMENT_SIZE: usize = 128 * 1024;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionTx {
    pub id: Option<u32>,
    pub hash: [u8; 32],
    pub state: TransactionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionState {
    Pending,
    Finalized(DateTime<Utc>),
    Failed,
}

pub struct TransactionResponse {
    block_number: u64,
    succeeded: bool,
}

impl TransactionResponse {
    pub fn new(block_number: u64, succeeded: bool) -> Self {
        Self {
            block_number,
            succeeded,
        }
    }

    pub fn block_number(&self) -> u64 {
        self.block_number
    }

    pub fn succeeded(&self) -> bool {
        self.succeeded
    }
}
