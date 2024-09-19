pub type FuelBlockHeight = u32;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BlockSubmissionTx {
    pub hash: [u8; 32],
    pub nonce: u64,
    pub max_fee: u64,
    pub priority_fee: u64,
    pub block_hash: [u8; 32],
    pub block_height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSubmission {
    pub block_hash: [u8; 32],
    pub block_height: FuelBlockHeight,
    pub final_tx_id: Option<u32>,
}

#[cfg(feature = "test-helpers")]
impl rand::distributions::Distribution<BlockSubmission> for rand::distributions::Standard {
    fn sample<R: rand::Rng + ?Sized>(&self, rng: &mut R) -> BlockSubmission {
        BlockSubmission {
            block_hash: rng.gen(),
            block_height: rng.gen(),
            final_tx_id: rng.gen(),
        }
    }
}
