use sqlx::types::chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispersalStatus {
    Processing,
    Confirmed,
    Finalized,
    Failed,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EigenDASubmission {
    pub id: Option<u64>,
    pub request_id: Vec<u8>,
    pub created_at: Option<DateTime<Utc>>,
    pub status: DispersalStatus,
}

impl Default for EigenDASubmission {
    fn default() -> Self {
        Self {
            id: None,
            request_id: [0; 32].to_vec(),
            status: DispersalStatus::Processing,
            created_at: None,
        }
    }
}
