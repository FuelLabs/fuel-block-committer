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
