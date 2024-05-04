use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct FuelBlock {
    pub hash: [u8; 32],
    pub height: u32,
}

#[cfg(feature = "test-helpers")]
impl rand::distributions::Distribution<FuelBlock> for rand::distributions::Standard {
    fn sample<R: rand::prelude::Rng + ?Sized>(&self, rng: &mut R) -> FuelBlock {
        FuelBlock {
            hash: rng.gen(),
            height: rng.gen(),
        }
    }
}

impl std::fmt::Debug for FuelBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hash = self.hash.map(|byte| format!("{byte:02x?}")).join("");
        f.debug_struct("FuelBlock")
            .field("hash", &hash)
            .field("height", &self.height)
            .finish()
    }
}
