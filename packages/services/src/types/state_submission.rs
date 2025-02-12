pub use sqlx::types::chrono::{DateTime, Utc};

use super::TransactionState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DASubmission<D: Serialize> {
    pub id: Option<u64>,
    pub hash: [u8; 32],
    pub created_at: Option<DateTime<Utc>>,
    pub state: TransactionState,
    pub details: D,
}

impl<D: Default + Serialize> Default for DASubmission<D> {
    fn default() -> Self {
        Self {
            id: None,
            hash: [0; 32],
            created_at: None,
            state: TransactionState::Pending,
            details: D::default(),
        }
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EthereumDetails {
    pub nonce: u32,
    pub max_fee: u128,
    pub priority_fee: u128,
    pub blob_fee: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EigenDetails {
    // TODO
}

pub type EthereumDASubmission = DASubmission<EthereumDetails>;
pub type EigenDASubmission = DASubmission<EigenDetails>;
