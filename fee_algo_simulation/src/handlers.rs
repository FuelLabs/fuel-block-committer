use super::models::{FeeDataPoint, FeeParams, FeeResponse, FeeStats};
use super::state::AppState;
use super::utils::last_n_blocks;
use actix_web::http::StatusCode;
use actix_web::{body, web, HttpResponse, HttpResponseBuilder, Responder};

use services::historical_fees::port::l1::Api;
use services::historical_fees::service::calculate_blob_tx_fee;

use services::state_committer::{AlgoConfig, FeeMultiplierRange, SmaFeeAlgo};
use tracing::{error, info};

use std::num::{NonZeroU32, NonZeroU64};
use std::time::Duration;

/// Handler for the root `/` endpoint, serving the HTML page.
pub async fn index_html() -> impl Responder {
    let contents = include_str!("index.html");
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(contents)
}

macro_rules! ok_or_bail {
    ($e:expr, $http_code: expr) => {
        match $e {
            Ok(val) => val,
            Err(e) => {
                error!("Error: {:?}", e);

                return HttpResponseBuilder::new($http_code)
                    .status($http_code)
                    .body(e.to_string());
            }
        }
    };
    () => {};
}

/// Handler for the `/fees` endpoint.
pub async fn get_fees(state: web::Data<AppState>, params: web::Query<FeeParams>) -> impl Responder {
    // Resolve user inputs or use defaults
    let ending_height = if let Some(val) = params.ending_height {
        val
    } else {
        ok_or_bail!(
            state.caching_api.current_height().await,
            StatusCode::INTERNAL_SERVER_ERROR
        )
    };
    let start_height = ending_height.saturating_sub(params.amount_of_blocks);

    let config = ok_or_bail!(
        AlgoConfig::try_from(params.clone().into_inner()),
        StatusCode::BAD_REQUEST
    );

    let sma_algo = SmaFeeAlgo::new(state.historical_fees.clone(), config);

    let seq_fees = ok_or_bail!(
        state.caching_api.fees(start_height..=ending_height).await,
        StatusCode::INTERNAL_SERVER_ERROR
    );

    let last_block_height = seq_fees.last().height;

    let last_block_time = {
        let resp = state
            .caching_api
            .inner()
            .get_block_time(last_block_height)
            .await;

        ok_or_bail!(resp, StatusCode::INTERNAL_SERVER_ERROR)
            .expect("to always be able to fetch the latest block")
    };

    info!("Last block time: {}", last_block_time);

    // Prepare data points
    let mut data = Vec::with_capacity(seq_fees.len());

    for block_fees in seq_fees.into_iter() {
        let block_height = block_fees.height;
        let current_fee_wei = calculate_blob_tx_fee(params.num_blobs, &block_fees.fees);

        let short_fee_wei = {
            let short_term_sma = ok_or_bail!(
                state
                    .historical_fees
                    .calculate_sma(last_n_blocks(block_height, config.sma_periods.short))
                    .await,
                StatusCode::INTERNAL_SERVER_ERROR
            );
            calculate_blob_tx_fee(params.num_blobs, &short_term_sma)
        };

        let long_fee_wei = {
            let long_term_sma = ok_or_bail!(
                state
                    .historical_fees
                    .calculate_sma(last_n_blocks(block_height, config.sma_periods.long))
                    .await,
                StatusCode::INTERNAL_SERVER_ERROR
            );
            calculate_blob_tx_fee(params.num_blobs, &long_term_sma)
        };

        let acceptable = ok_or_bail!(
            sma_algo
                .fees_acceptable(params.num_blobs, params.num_l2_blocks_behind, block_height)
                .await,
            StatusCode::INTERNAL_SERVER_ERROR
        );

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
