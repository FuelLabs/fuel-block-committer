#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubmission {
    pub block_hash: [u8; 32],
    pub block_height: u32,
    pub completed: bool,
}

pub type StateFragmentId = ([u8; 32], u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFragment {
    pub block_hash: [u8; 32],
    pub transaction_hash: Option<[u8; 32]>,
    pub fragment_index: u32,
    pub raw_data: Vec<u8>,
    pub completed: bool,
}

impl StateFragment {
    pub const MAX_FRAGMENT_SIZE: usize = 128 * 1024;

    pub fn id(&self) -> StateFragmentId {
        (self.block_hash, self.fragment_index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubmissionTx {
    pub hash: [u8; 32],
    pub state: TransactionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionState {
    Pending,
    Finalized,
}

// Used for DB storage
impl TransactionState {
    pub fn into_i16(&self) -> i16 {
        match self {
            TransactionState::Pending => 0,
            TransactionState::Finalized => 1,
        }
    }

    pub fn from_i16(value: i16) -> Option<Self> {
        match value {
            0 => Some(Self::Pending),
            1 => Some(Self::Finalized),
            _ => None,
        }
    }
}
