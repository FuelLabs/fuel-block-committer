use std::time::Duration;

use actix_web::{http::StatusCode, web, HttpResponse, HttpResponseBuilder, Responder};
use anyhow::Result;
use eth::HttpClient;
use futures::{stream, StreamExt};
use itertools::Itertools;
use services::{
    fee_metrics_tracker::{
        port::{
            cache::CachingApi,
            l1::{Api, SequentialBlockFees},
        },
        service::calculate_blob_tx_fee,
    },
    state_committer::{AlgoConfig, SmaFeeAlgo},
    types::{DateTime, Utc},
};
use tracing::{error, info};

use super::{
    models::{FeeDataPoint, FeeParams, FeeResponse, FeeStats},
    state::AppState,
    utils::last_n_blocks,
};

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
            state.fee_api.current_height().await,
            StatusCode::INTERNAL_SERVER_ERROR
        )
    };
    let start_height = ending_height.saturating_sub(params.amount_of_blocks);

    let config = ok_or_bail!(
        AlgoConfig::try_from(params.clone().into_inner()),
        StatusCode::BAD_REQUEST
    );

    let seq_fees = ok_or_bail!(
        state.fee_api.fees(start_height..=ending_height).await,
        StatusCode::INTERNAL_SERVER_ERROR
    );

    let last_block_height = seq_fees.last().height;

    let last_block_time = {
        let resp = state
            .fee_api
            .inner()
            .get_block_time(last_block_height)
            .await;

        ok_or_bail!(resp, StatusCode::INTERNAL_SERVER_ERROR)
            .expect("to always be able to fetch the latest block")
    };

    info!("Last block time: {}", last_block_time);

    // Prepare data points

    let data = ok_or_bail!(
        calc_data(
            seq_fees,
            params.0,
            config,
            state.fee_api.clone(),
            last_block_height,
            last_block_time
        )
        .await,
        StatusCode::INTERNAL_SERVER_ERROR
    );

    // Calculate statistics
    let stats = calculate_statistics(&data);

    let response = FeeResponse { data, stats };

    // Return as JSON
    HttpResponse::Ok().json(response)
}

async fn calc_data(
    seq_fees: SequentialBlockFees,
    params: FeeParams,
    config: AlgoConfig,
    fee_api: CachingApi<HttpClient>,
    last_block_height: u64,
    last_block_time: DateTime<Utc>,
) -> Result<Vec<FeeDataPoint>> {
    let sma_algo = SmaFeeAlgo::new(fee_api.clone(), config);

    let mut data = vec![];

    for services::fee_metrics_tracker::port::l1::BlockFees { height, fees } in seq_fees {
        let current_fee_wei = calculate_blob_tx_fee(params.num_blobs, &fees);

        let fetch_tx_fee = |n: std::num::NonZeroU64| {
            let fee_api = fee_api.clone();
            async move {
                let fees = fee_api.fees(last_n_blocks(height, n)).await?.mean();
                anyhow::Result::<_>::Ok(calculate_blob_tx_fee(params.num_blobs, &fees))
            }
        };

        let short_fee_wei = fetch_tx_fee(config.sma_periods.short).await?;
        let long_fee_wei = fetch_tx_fee(config.sma_periods.long).await?;
        let acceptable = sma_algo
            .fees_acceptable(params.num_blobs, params.num_l2_blocks_behind, height)
            .await?;

        let block_gap = last_block_height - height;
        let block_time = last_block_time - Duration::from_secs(12 * block_gap);

        let convert = |wei| format!("{:.4}", (wei as f64) / 1e18);
        data.push(FeeDataPoint {
            block_height: height,
            block_time: block_time.to_rfc3339(),
            current_fee: convert(current_fee_wei),
            short_fee: convert(short_fee_wei),
            long_fee: convert(long_fee_wei),
            acceptable,
        })
    }
    Ok(data)
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
