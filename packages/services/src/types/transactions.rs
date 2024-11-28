use sqlx::types::chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionState {
    Pending,
    IncludedInBlock,
    Finalized(DateTime<Utc>),
    SqueezedOut,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionResponse {
    block_number: u64,
    succeeded: bool,
    fee: u128,
    blob_fee: u128,
}

impl TransactionResponse {
    pub fn new(block_number: u64, succeeded: bool, fee: u128, blob_fee: u128) -> Self {
        Self {
            block_number,
            succeeded,
            fee,
            blob_fee,
        }
    }

    pub fn block_number(&self) -> u64 {
        self.block_number
    }

    pub fn succeeded(&self) -> bool {
        self.succeeded
    }

    pub fn total_fee(&self) -> u128 {
        self.fee.saturating_add(self.blob_fee)
    }

    pub fn confirmations(&self, current_block_number: u64) -> u64 {
        if !self.succeeded() {
            return 0;
        }

        current_block_number.saturating_sub(self.block_number)
    }
}
