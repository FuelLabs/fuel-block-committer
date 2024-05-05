use crate::types::{EthHeight, FuelBlock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSubmission {
    pub block: FuelBlock,
    pub completed: bool,
    // Eth block height moments before submitting the fuel block. Used to filter stale events in
    // the commit listener.
    pub submittal_height: EthHeight,
}

#[cfg(feature = "test-helpers")]
impl rand::distributions::Distribution<BlockSubmission> for rand::distributions::Standard {
    fn sample<R: rand::Rng + ?Sized>(&self, rng: &mut R) -> BlockSubmission {
        BlockSubmission {
            block: rng.gen(),
            completed: rng.gen(),
            submittal_height: rng.gen(),
        }
    }
}
