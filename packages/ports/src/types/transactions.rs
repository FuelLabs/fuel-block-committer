#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionState {
    Pending,
    Finalized,
    Failed,
}

// Used for DB storage
impl TransactionState {
    pub fn into_i16(&self) -> i16 {
        match self {
            TransactionState::Pending => 0,
            TransactionState::Finalized => 1,
            TransactionState::Failed => 2,
        }
    }

    pub fn from_i16(value: i16) -> Option<Self> {
        match value {
            0 => Some(Self::Pending),
            1 => Some(Self::Finalized),
            2 => Some(Self::Failed),
            _ => None,
        }
    }
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
