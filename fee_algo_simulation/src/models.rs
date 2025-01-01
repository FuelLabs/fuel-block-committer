use serde::{Deserialize, Serialize};
use services::historical_fees::port::l1::BlockFees;

/// Ethereum RPC URL.
pub const URL: &str = "https://eth.llamarpc.com";

/// Structure for saving fees to cache.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct SavedFees {
    pub fees: Vec<BlockFees>,
}

/// Query parameters for the `/fees` endpoint.
#[derive(Debug, Deserialize)]
pub struct FeeParams {
    pub ending_height: Option<u64>,
    pub amount_of_blocks: u64,
    pub short: u64,
    pub long: u64,
    pub max_l2_blocks_behind: u32,
    pub start_max_fee_multiplier: f64,
    pub end_max_fee_multiplier: f64,
    pub always_acceptable_fee: String,
    pub num_blobs: u32,
    pub num_l2_blocks_behind: u32,
}

/// Response struct for each fee data point.
#[derive(Debug, Serialize)]
pub struct FeeDataPoint {
    #[serde(rename = "blockHeight")]
    pub block_height: u64,

    #[serde(rename = "blockTime")]
    pub block_time: String, // ISO 8601 format

    #[serde(rename = "currentFee")]
    pub current_fee: String, // ETH with 4 decimal places

    #[serde(rename = "shortFee")]
    pub short_fee: String, // ETH with 4 decimal places

    #[serde(rename = "longFee")]
    pub long_fee: String, // ETH with 4 decimal places

    pub acceptable: bool,
}

/// Statistics struct.
#[derive(Debug, Serialize)]
pub struct FeeStats {
    #[serde(rename = "percentageAcceptable")]
    pub percentage_acceptable: f64, // Percentage of acceptable blocks

    #[serde(rename = "percentile95GapSize")]
    pub percentile_95_gap_size: u64, // 95th percentile of gap sizes in blocks

    #[serde(rename = "longestUnacceptableStreak")]
    pub longest_unacceptable_streak: u64, // Longest consecutive unacceptable blocks
}

/// Complete response struct.
#[derive(Debug, Serialize)]
pub struct FeeResponse {
    pub data: Vec<FeeDataPoint>,
    pub stats: FeeStats,
}
