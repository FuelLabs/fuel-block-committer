use serde::{Deserialize, Serialize};

pub struct TransactionCostUpdate {
    pub tx_hash: [u8; 32],
    pub total_fee: u128,
    pub da_block_height: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BundleCost {
    // total cost of the bundle
    pub cost: u128,
    // total size of the data contained in the bundle
    pub size: u64,
    // da height of the final transaction carrying the bundle
    pub da_block_height: u64,
    // starting height of the block contained block range
    pub start_height: u64,
    // ending height of the block contained block range (inclusive)
    pub end_height: u64,
}
