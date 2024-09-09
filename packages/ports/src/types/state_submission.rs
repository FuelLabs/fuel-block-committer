use std::ops::Range;

pub use sqlx::types::chrono::{DateTime, Utc};

use super::NonNegativeI32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubmission {
    pub id: Option<NonNegativeI32>,
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
    start: NonNegativeI32,
    end: NonNegativeI32,
}

impl TryFrom<Range<u32>> for ValidatedRange {
    type Error = InvalidRange;

    fn try_from(range: Range<u32>) -> Result<Self, Self::Error> {
        if range.start > range.end {
            return Err(Self::Error {
                message: format!(
                    "start ({}) must be less than or equal to end ({})",
                    range.start, range.end
                ),
            });
        }

        let start = NonNegativeI32::try_from(range.start).map_err(|e| InvalidRange {
            message: e.to_string(),
        })?;
        let end = NonNegativeI32::try_from(range.end).map_err(|e| InvalidRange {
            message: e.to_string(),
        })?;

        Ok(Self { start, end })
    }
}

impl From<ValidatedRange> for Range<u32> {
    fn from(value: ValidatedRange) -> Self {
        value.start.as_u32()..value.end.as_u32()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFragment {
    pub submission_id: u64,
    pub data_range: ValidatedRange,
    pub created_at: DateTime<Utc>,
}

impl StateFragment {
    pub const MAX_FRAGMENT_SIZE: usize = 128 * 1024;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionTx {
    pub id: Option<u64>,
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
