pub use sqlx::types::chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubmission {
    pub id: Option<u32>,
    pub block_hash: [u8; 32],
    pub block_height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFragment {
    pub id: Option<u32>,
    pub submission_id: Option<u32>,
    pub fragment_idx: u32,
    pub data: Vec<u8>,
    pub created_at: DateTime<Utc>,
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
    Finalized,
    Failed,
}

// Used for DB storage
impl TransactionState {
    pub fn into_i16(&self) -> i16 {
        match self {
            TransactionState::Pending => 0,
            TransactionState::Finalized => 1,
            TransactionState::Failed => 2,
        }
    }

    pub fn from_i16(value: i16) -> Option<Self> {
        match value {
            0 => Some(Self::Pending),
            1 => Some(Self::Finalized),
            2 => Some(Self::Failed),
            _ => None,
        }
    }
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
