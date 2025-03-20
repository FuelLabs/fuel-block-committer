use std::num::{NonZeroU32, NonZeroU64};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use services::{
    fees::FeesAtHeight,
    state_committer::{AlgoConfig, FeeMultiplierRange, FeeThresholds, SmaPeriods},
};

pub const URL: &str = "https://eth.llamarpc.com";

/// Structure for saving fees to cache.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct SavedFees {
    pub fees: Vec<FeesAtHeight>,
}

/// Query parameters for the `/fees` endpoint.
#[derive(Clone, Debug, Deserialize)]
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
    #[serde(default)]
    pub num_l2_blocks_behind: u32,
}

impl TryFrom<FeeParams> for AlgoConfig {
    type Error = anyhow::Error;

    fn try_from(value: FeeParams) -> Result<Self, Self::Error> {
        let short = NonZeroU64::new(value.short).context("short sma period must be non-zero")?;
        let long = NonZeroU64::new(value.long).context("long sma period must be non-zero")?;

        let sma_periods = SmaPeriods { short, long };

        let max_l2_blocks_behind = NonZeroU32::new(value.max_l2_blocks_behind)
            .context("max_l2_blocks_behind must be non-zero")?;

        let multiplier_range =
            FeeMultiplierRange::new(value.start_max_fee_multiplier, value.end_max_fee_multiplier)?;

        let always_acceptable_fee = value
            .always_acceptable_fee
            .parse()
            .context("always_acceptable_fee must be a valid u128")?;

        Ok(AlgoConfig {
            sma_periods,
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind,
                multiplier_range,
                always_acceptable_fee,
            },
        })
    }
}

/// Response struct for each fee data point.
#[derive(Debug, Serialize)]
pub struct FeeDataPoint {
    #[serde(rename = "blockHeight")]
    pub block_height: u64,

    #[serde(rename = "currentFee")]
    pub current_fee: String, // ETH with 4 decimal places

    #[serde(rename = "shortFee")]
    pub short_fee: String, // ETH with 4 decimal places

    #[serde(rename = "longFee")]
    pub long_fee: String, // ETH with 4 decimal places

    pub acceptable: bool,
}

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
#[derive(Clone, Debug, Deserialize)]
pub struct SimulationParams {
    #[serde(flatten)]
    pub fee_params: FeeParams,
    pub bundling_interval_blocks: u32,
    pub bundle_blob_count: u32,
    pub finalization_time_minutes: u32,
}

#[derive(Debug, Serialize)]
pub struct SimulationPoint {
    pub block_height: u64,
    pub immediate_fee: f64,
    pub algorithm_fee: f64,
    pub immediate_l2_behind: u32,
    pub algo_l2_behind: u32,
}

/// The full simulation result.
#[derive(Debug, Serialize)]
pub struct SimulationResult {
    pub immediate_total_fee: f64,
    pub algorithm_total_fee: f64,
    pub eth_saved: f64,
    pub timeline: Vec<SimulationPoint>,
}
