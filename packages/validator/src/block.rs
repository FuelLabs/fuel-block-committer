use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedFuelBlock {
    pub(crate) hash: [u8; 32],
    pub(crate) height: u32,
}

impl ValidatedFuelBlock {
    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    #[cfg(feature = "test-helpers")]
    pub fn new(hash: [u8; 32], height: u32) -> Self {
        Self { hash, height }
    }
}

impl std::fmt::Debug for ValidatedFuelBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hash = self.hash.map(|byte| format!("{byte:02x?}")).join("");
        f.debug_struct("ValidatedFuelBlock")
            .field("hash", &hash)
            .field("height", &self.height)
            .finish()
    }
}

#[cfg(feature = "test-helpers")]
impl From<fuel_core_client::client::types::block::Block> for ValidatedFuelBlock {
    fn from(block: fuel_core_client::client::types::block::Block) -> Self {
        Self {
            hash: *block.id,
            height: block.header.height,
        }
    }
}

#[cfg(feature = "test-helpers")]
impl rand::distributions::Distribution<ValidatedFuelBlock> for rand::distributions::Standard {
    fn sample<R: rand::prelude::Rng + ?Sized>(&self, rng: &mut R) -> ValidatedFuelBlock {
        ValidatedFuelBlock {
            hash: rng.gen(),
            height: rng.gen(),
        }
    }
}
