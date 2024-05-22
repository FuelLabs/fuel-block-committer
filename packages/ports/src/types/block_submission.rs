use crate::types::L1Height;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSubmission {
    pub block_hash: [u8; 32],
    pub block_height: u32,
    pub completed: bool,
    // L1 block height moments before submitting the fuel block. Used to filter stale events in
    // the commit listener.
    pub submittal_height: L1Height,
}

#[cfg(feature = "test-helpers")]
impl rand::distributions::Distribution<BlockSubmission> for rand::distributions::Standard {
    fn sample<R: rand::Rng + ?Sized>(&self, rng: &mut R) -> BlockSubmission {
        BlockSubmission {
            block_hash: rng.gen(),
            block_height: rng.gen(),
            completed: rng.gen(),
            submittal_height: rng.gen(),
        }
    }
}
