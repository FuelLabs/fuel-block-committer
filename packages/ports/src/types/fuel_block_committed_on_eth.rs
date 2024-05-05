use crate::types::U256;

#[derive(Clone, Copy)]
pub struct FuelBlockCommittedOnEth {
    pub fuel_block_hash: [u8; 32],
    pub commit_height: U256,
}

impl std::fmt::Debug for FuelBlockCommittedOnEth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hash = self
            .fuel_block_hash
            .map(|byte| format!("{byte:02x?}"))
            .join("");
        f.debug_struct("FuelBlockCommittedOnEth")
            .field("hash", &hash)
            .field("commit_height", &self.commit_height)
            .finish()
    }
}
