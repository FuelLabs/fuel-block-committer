use std::ops::Range;

pub use sqlx::types::chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubmission {
    pub id: Option<u32>,
    pub block_hash: [u8; 32],
    pub block_height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct InvalidRange {
    pub message: String,
}

impl std::fmt::Display for InvalidRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid range: {}", self.message)
    }
}

impl std::error::Error for InvalidRange {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedRange {
    range: Range<u32>,
}

impl TryFrom<Range<u32>> for ValidatedRange {
    type Error = InvalidRange;

    fn try_from(range: Range<u32>) -> Result<Self, Self::Error> {
        if range.start > range.end {
            Err(Self::Error {
                message: format!(
                    "start ({}) must be less than or equal to end ({})",
                    range.start, range.end
                ),
            })
        } else {
            Ok(Self { range })
        }
    }
}

impl From<ValidatedRange> for Range<u32> {
    fn from(value: ValidatedRange) -> Self {
        value.range
    }
}

impl AsRef<Range<u32>> for ValidatedRange {
    fn as_ref(&self) -> &Range<u32> {
        &self.range
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFragment {
    pub id: Option<u32>,
    pub submission_id: Option<u32>,
    pub data_range: ValidatedRange,
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
