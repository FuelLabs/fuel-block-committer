use super::models::{FeeDataPoint, FeeParams, FeeResponse, FeeStats};
use super::state::AppState;
use super::utils::last_n_blocks;
use actix_web::{web, HttpResponse, Responder};

use services::historical_fees::port::l1::Api;
use services::historical_fees::service::calculate_blob_tx_fee;

use services::state_committer::FeeMultiplierRange;
use tracing::{error, info};

use std::fs;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::Path;
use std::time::Duration;

/// Handler for the root `/` endpoint, serving the HTML page.
pub async fn index_html() -> impl Responder {
    let contents = include_str!("index.html");
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(contents)
}

/// Handler for the `/fees` endpoint.
pub async fn get_fees(state: web::Data<AppState>, params: web::Query<FeeParams>) -> impl Responder {
    // Resolve user inputs or use defaults
    let ending_height = if let Some(val) = params.ending_height {
        val
    } else {
        match state.caching_api.current_height().await {
            Ok(height) => height,
            Err(e) => {
                error!("Failed to get current height: {:?}", e);
                return HttpResponse::InternalServerError().body("Failed to get current height");
            }
        }
    };
    let amount_of_blocks = params.amount_of_blocks;

    let short = params.short;
    let long = params.long;

    let max_l2 = params.max_l2_blocks_behind;
    let start_max_fee_multiplier = params.start_max_fee_multiplier;
    let end_max_fee_multiplier = params.end_max_fee_multiplier;

    let always_acceptable_fee = match params.always_acceptable_fee.parse() {
        Ok(val) => val,
        Err(_) => {
            return HttpResponse::BadRequest().body("Invalid always_acceptable_fee value");
        }
    };

    let num_blobs = params.num_blobs;

    let num_l2_blocks_behind = params.num_l2_blocks_behind;

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
            multiplier_range: match FeeMultiplierRange::new(
                start_max_fee_multiplier,
                end_max_fee_multiplier,
            ) {
                Ok(range) => range,
                Err(e) => {
                    error!("Invalid fee multiplier range: {:?}", e);
                    return HttpResponse::BadRequest().body("Invalid fee multiplier range");
                }
            },
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
        Err(e) => {
            error!(
                "Failed to fetch fees for range {}-{}: {:?}",
                start_height, ending_height, e
            );
            return HttpResponse::InternalServerError().body("Failed to fetch fees");
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
            return HttpResponse::InternalServerError()
                .body("Failed to retrieve the last block's time");
        }
        Err(e) => {
            error!("Error while fetching the last block's time: {:?}", e);
            return HttpResponse::InternalServerError()
                .body("Error while fetching the last block's time");
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
        let block_time = last_block_time - Duration::from_secs(12 * block_gap as u64); // Assuming 12 seconds per block
        let block_time_str = block_time.to_rfc3339();

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
    HttpResponse::Ok().json(response)
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
