use crate::types::U256;

#[derive(Clone, Copy)]
pub struct FuelBlockCommittedOnL1 {
    pub fuel_block_hash: [u8; 32],
    pub commit_height: U256,
}

impl std::fmt::Debug for FuelBlockCommittedOnL1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hash = self
            .fuel_block_hash
            .map(|byte| format!("{byte:02x?}"))
            .join("");
        f.debug_struct("FuelBlockCommittedOnL1")
            .field("hash", &hash)
            .field("commit_height", &self.commit_height)
            .finish()
    }
}
