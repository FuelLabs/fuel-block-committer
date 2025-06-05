use base64::Engine;
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

pub trait AsB64 {
    fn as_base64(&self) -> String;
}

impl AsB64 for EigenDASubmission {
    fn as_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(&self.request_id)
    }
}
