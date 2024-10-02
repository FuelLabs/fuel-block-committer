use sqlx::types::chrono::{DateTime, Utc};

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

    pub fn confirmations(&self, current_block_number: u64) -> u64 {
        if !self.succeeded() {
            return 0;
        }

        current_block_number.saturating_sub(self.block_number)
    }
}
