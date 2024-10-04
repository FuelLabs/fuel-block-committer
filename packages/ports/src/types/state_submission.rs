pub use sqlx::types::chrono::{DateTime, Utc};

use super::NonNegative;

use super::TransactionState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubmission {
    pub id: Option<NonNegative<i32>>,
    pub block_hash: [u8; 32],
    pub block_height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L1Tx {
    pub id: Option<u64>,
    pub hash: [u8; 32],
    pub state: TransactionState,
}
