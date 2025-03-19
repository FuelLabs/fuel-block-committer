use std::num::{NonZeroU32, NonZeroU64};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_aux::prelude::*;
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
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub always_acceptable_fee: u128,
    pub num_blobs: u32,
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

        Ok(AlgoConfig {
            sma_periods,
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind,
                multiplier_range,
                always_acceptable_fee: value.always_acceptable_fee,
            },
        })
    }
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
    #[serde(default = "default_blob_count")]
    pub blob_count: u32,
    #[serde(default = "default_blob_interval_minutes")]
    pub blob_interval_minutes: u32,
}

fn default_blob_count() -> u32 {
    6
}

fn default_blob_interval_minutes() -> u32 {
    60
}

/// A point on the simulation timeline.
#[derive(Debug, Serialize)]
pub struct SimulationPoint {
    pub time: String,       // ISO 8601
    pub immediate_fee: f64, // cumulative fee (ETH) if blobs were committed immediately
    pub algorithm_fee: f64, // cumulative fee (ETH) using the algorithm-driven mode
    pub backlog: u32,       // number of blobs waiting to be committed
}

/// The full simulation result.
#[derive(Debug, Serialize)]
pub struct SimulationResult {
    pub immediate_total_fee: f64,
    pub algorithm_total_fee: f64,
    pub eth_saved: f64,
    pub timeline: Vec<SimulationPoint>,
}
