pub use sqlx::types::chrono::{DateTime, Utc};

use super::TransactionState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L1Tx {
    pub id: Option<u64>,
    pub hash: [u8; 32],
    pub nonce: u32,
    pub max_fee: u128,
    pub priority_fee: u128,
    pub blob_fee: u128,
    pub created_at: Option<DateTime<Utc>>,
    pub state: TransactionState,
}

impl Default for L1Tx {
    fn default() -> Self {
        Self {
            id: None,
            hash: [0; 32],
            nonce: 0,
            max_fee: 0,
            priority_fee: 0,
            blob_fee: 0,
            state: TransactionState::Pending,
            created_at: None,
        }
    }
}
