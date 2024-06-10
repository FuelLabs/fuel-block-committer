pub struct StateSubmission {
    pub block_hash: [u8; 32],
    pub block_height: u32,
    pub completed: bool,
}

pub struct StateFragment {
    pub fragment_index: u32,
    pub block_hash: [u8; 32],
    pub raw_data: Vec<u8>,
    pub completed: bool,
}
