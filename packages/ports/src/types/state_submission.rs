pub struct StateSubmission {
    pub fuel_block_height: u32,
    pub is_completed: bool,
    pub num_fragments: u32,
}

pub struct StateFragment {
    pub fragment_index: u32,
    pub state_submission: u32,
    pub raw_data: Vec<u8>,
    pub is_completed: bool,
}
