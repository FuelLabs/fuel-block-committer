use sqlx::types::chrono::{DateTime, Utc};

use super::TransactionState;

pub type FuelBlockHeight = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSubmissionTx {
    pub id: Option<u32>,
    pub submission_id: Option<u32>,
    pub hash: [u8; 32],
    pub nonce: u32,
    pub max_fee: u128,
    pub priority_fee: u128,
    pub state: TransactionState,
    pub created_at: DateTime<Utc>,
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
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSubmission {
    pub id: Option<u32>,
    pub block_hash: [u8; 32],
    pub block_height: FuelBlockHeight,
    pub final_tx_id: Option<u32>,
}

impl BlockSubmission {
    pub fn new(block_hash: [u8; 32], block_height: FuelBlockHeight) -> Self {
        Self {
            id: None,
            final_tx_id: None,
            block_hash,
            block_height,
        }
    }
}

#[cfg(feature = "test-helpers")]
impl rand::distributions::Distribution<BlockSubmission> for rand::distributions::Standard {
    fn sample<R: rand::Rng + ?Sized>(&self, rng: &mut R) -> BlockSubmission {
        BlockSubmission {
            id: Some(rng.gen()),
            block_hash: rng.gen(),
            block_height: rng.gen(),
            final_tx_id: rng.gen(),
        }
    }
}
