use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct BundleCost {
    pub cost: u128,
    pub size: u64,
    pub da_block_height: u64,
    pub start_height: u64,
    pub end_height: u64,
}
