use std::num::{NonZeroU32, NonZeroU64};
use std::time::Duration;

use super::models::{FeeDataPoint, FeeParams, FeeResponse, FeeStats};
use super::state::AppState;
use super::utils::last_n_blocks;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::Json;
use services::historical_fees::port::l1::Api;
use services::historical_fees::service::calculate_blob_tx_fee;

use services::state_committer::FeeMultiplierRange;
use tracing::{error, info};

/// Handler for the `/fees` endpoint.
pub async fn get_fees(
    State(state): State<AppState>,
    Query(params): Query<FeeParams>,
) -> impl IntoResponse {
    // Resolve user inputs or use defaults
    let ending_height = params.ending_height.unwrap_or(21_514_918);
    let amount_of_blocks = params
        .amount_of_blocks
        .unwrap_or(state.num_blocks_per_month);

    let short = params.short.unwrap_or(25); // default short
    let long = params.long.unwrap_or(300); // default long

    let max_l2 = params.max_l2_blocks_behind.unwrap_or(8 * 3600);
    let start_max_fee_multiplier = params.start_max_fee_multiplier.unwrap_or(0.80);
    let end_max_fee_multiplier = params.end_max_fee_multiplier.unwrap_or(1.20);

    let always_acceptable_fee = match params.always_acceptable_fee {
        Some(v) => match v.parse::<u128>() {
            Ok(val) => val,
            Err(_) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    "Invalid always_acceptable_fee value",
                )
                    .into_response()
            }
        },
        None => 1_000_000_000_000_000,
    };

    let num_blobs = params.num_blobs.unwrap_or(6); // default to 6 blobs

    let num_l2_blocks_behind = params.num_l2_blocks_behind.unwrap_or(0);

    // Build SmaFeeAlgo config from user’s inputs
    let config = services::state_committer::AlgoConfig {
        sma_periods: services::historical_fees::service::SmaPeriods {
            short: match NonZeroU64::new(short) {
                Some(nz) => nz,
                None => NonZeroU64::new(1).unwrap(),
            },
            long: match NonZeroU64::new(long) {
                Some(nz) => nz,
                None => NonZeroU64::new(1).unwrap(),
            },
        },
        fee_thresholds: services::state_committer::FeeThresholds {
            max_l2_blocks_behind: match NonZeroU32::new(max_l2) {
                Some(nz) => nz,
                None => NonZeroU32::new(1).unwrap(),
            },
            multiplier_range: FeeMultiplierRange::new(
                start_max_fee_multiplier,
                end_max_fee_multiplier,
            )
            .unwrap(),
            always_acceptable_fee,
        },
    };

    let sma_algo =
        services::state_committer::SmaFeeAlgo::new(state.historical_fees.clone(), config);

    // Determine which blocks to fetch
    let start_height = ending_height.saturating_sub(amount_of_blocks);
    let range = start_height..=ending_height;

    // Fetch fees from the caching API
    let fees_res = state.caching_api.fees(range).await;
    let seq_fees = match fees_res {
        Ok(fees) => fees,
        Err(_) => {
            error!(
                "Failed to fetch fees for range {}-{}",
                start_height, ending_height
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch fees").into_response();
        }
    };

    // Fetch the last block's time
    let last_block_height = seq_fees.last().height;
    let last_block_time = match state
        .caching_api
        .inner()
        .get_block_time(last_block_height)
        .await
    {
        Ok(Some(t)) => t, // Assuming `t` is a UNIX timestamp in seconds
        Ok(None) => {
            error!(
                "Failed to retrieve the last block's time for block height {}",
                last_block_height
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to retrieve the last block's time",
            )
                .into_response();
        }
        Err(e) => {
            error!("Error while fetching the last block's time: {:?}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error while fetching the last block's time",
            )
                .into_response();
        }
    };

    info!("Last block time: {}", last_block_time);

    // Prepare data points
    let mut data = Vec::with_capacity(seq_fees.len());

    for block_fees in seq_fees.into_iter() {
        let block_height = block_fees.height;
        let current_fee_wei = calculate_blob_tx_fee(num_blobs, &block_fees.fees);

        // Fetch the shortTerm + longTerm SMA at exactly this height
        let short_term_sma = match state
            .historical_fees
            .calculate_sma(last_n_blocks(block_height, config.sma_periods.short))
            .await
        {
            Ok(f) => f,
            Err(e) => {
                error!(
                    "Error calculating short-term SMA for block {}: {:?}",
                    block_height, e
                );
                services::historical_fees::port::l1::Fees::default()
            }
        };

        let long_term_sma = match state
            .historical_fees
            .calculate_sma(last_n_blocks(block_height, config.sma_periods.long))
            .await
        {
            Ok(f) => f,
            Err(e) => {
                error!(
                    "Error calculating long-term SMA for block {}: {:?}",
                    block_height, e
                );
                services::historical_fees::port::l1::Fees::default()
            }
        };

        let short_fee_wei = calculate_blob_tx_fee(num_blobs, &short_term_sma);
        let long_fee_wei = calculate_blob_tx_fee(num_blobs, &long_term_sma);

        let acceptable = match sma_algo
            .fees_acceptable(num_blobs, num_l2_blocks_behind, block_height)
            .await
        {
            Ok(decision) => decision,
            Err(e) => {
                error!(
                    "Error determining fee acceptability for block {}: {:?}",
                    block_height, e
                );
                false // or handle error differently
            }
        };

        // Calculate the time for this block
        let block_gap = last_block_height - block_height;
        let block_time = last_block_time - Duration::from_secs(12 * block_gap);
        let block_time_str = block_time.to_rfc3339(); // ISO 8601 format

        // Convert fees from wei to ETH with 4 decimal places
        let current_fee_eth = (current_fee_wei as f64) / 1e18;
        let short_fee_eth = (short_fee_wei as f64) / 1e18;
        let long_fee_eth = (long_fee_wei as f64) / 1e18;

        data.push(FeeDataPoint {
            block_height,
            block_time: block_time_str,
            current_fee: format!("{:.4}", current_fee_eth),
            short_fee: format!("{:.4}", short_fee_eth),
            long_fee: format!("{:.4}", long_fee_eth),
            acceptable,
        });
    }

    // Calculate statistics
    let stats = calculate_statistics(&data);

    let response = FeeResponse { data, stats };

    // Return as JSON
    Json(response).into_response()
}

/// Handler for the root `/` endpoint, serving the HTML page.
pub async fn index_html() -> Html<&'static str> {
    // The HTML content is stored in `index.html` and included at compile time.
    Html(include_str!("./index.html"))
}

/// Calculates statistics from the fee data.
fn calculate_statistics(data: &Vec<FeeDataPoint>) -> FeeStats {
    let total_blocks = data.len() as f64;
    let acceptable_blocks = data.iter().filter(|d| d.acceptable).count() as f64;
    let percentage_acceptable = if total_blocks > 0.0 {
        (acceptable_blocks / total_blocks) * 100.0
    } else {
        0.0
    };

    // Calculate gap sizes (streaks of unacceptable blocks)
    let mut gap_sizes = Vec::new();
    let mut current_gap = 0;

    for d in data {
        if !d.acceptable {
            current_gap += 1;
        } else if current_gap > 0 {
            gap_sizes.push(current_gap);
            current_gap = 0;
        }
    }

    // Push the last gap if it ends with an unacceptable streak
    if current_gap > 0 {
        gap_sizes.push(current_gap);
    }

    // Calculate the 95th percentile of gap sizes
    let percentile_95_gap_size = if !gap_sizes.is_empty() {
        let mut sorted_gaps = gap_sizes.clone();
        sorted_gaps.sort_unstable();
        let index = ((sorted_gaps.len() as f64) * 0.95).ceil() as usize - 1;
        sorted_gaps[index.min(sorted_gaps.len() - 1)]
    } else {
        0
    };

    // Find the longest unacceptable streak
    let longest_unacceptable_streak = gap_sizes.iter().cloned().max().unwrap_or(0);

    FeeStats {
        percentage_acceptable,
        percentile_95_gap_size,
        longest_unacceptable_streak,
    }
}
