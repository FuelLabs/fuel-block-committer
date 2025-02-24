use sqlx::types::chrono::{DateTime, Utc};

use super::{NonEmpty, NonNegative, TransactionState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressedFuelBlock {
    pub height: u32,
    pub data: NonEmpty<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSubmissionTx {
    pub id: Option<NonNegative<i32>>,
    pub submission_id: Option<NonNegative<i32>>,
    pub hash: [u8; 32],
    pub nonce: u32,
    pub max_fee: u128,
    pub priority_fee: u128,
    pub state: TransactionState,
    pub created_at: Option<DateTime<Utc>>,
}

impl Default for BlockSubmissionTx {
    fn default() -> Self {
        Self {
            id: None,
            submission_id: None,
            hash: [0; 32],
            nonce: 0,
            max_fee: 0,
            priority_fee: 0,
            state: TransactionState::Pending,
            created_at: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSubmission {
    pub id: Option<NonNegative<i32>>,
    pub block_hash: [u8; 32],
    pub block_height: u32,
    pub completed: bool,
}

impl BlockSubmission {
    pub fn new(block_hash: [u8; 32], block_height: u32) -> Self {
        Self {
            id: None,
            block_hash,
            block_height,
            completed: false,
        }
    }
}

#[cfg(feature = "test-helpers")]
impl rand::distributions::Distribution<BlockSubmission> for rand::distributions::Standard {
    fn sample<R: rand::Rng + ?Sized>(&self, rng: &mut R) -> BlockSubmission {
        BlockSubmission {
            id: Some(rng.r#gen()),
            block_hash: rng.r#gen(),
            block_height: rng.r#gen(),
            completed: rng.r#gen(),
        }
    }
}
