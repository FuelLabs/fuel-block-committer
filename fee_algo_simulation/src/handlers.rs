use actix_web::{HttpResponse, Responder, ResponseError, web};
use anyhow::Result;
use eth::HttpClient;
use itertools::Itertools;
use serde_json::json;
use services::{
    fee_metrics_tracker::service::calculate_blob_tx_fee,
    fees::{Api, FeesAtHeight, SequentialBlockFees, cache::CachingApi},
    state_committer::{AlgoConfig, SmaFeeAlgo},
    types::{DateTime, Utc},
};
use thiserror::Error;
use tracing::{error, info};

use crate::models::{SimulationParams, SimulationPoint, SimulationResult};

use super::{
    models::{FeeDataPoint, FeeParams, FeeResponse, FeeStats},
    state::AppState,
    utils::last_n_blocks,
};

#[derive(Error, Debug)]
pub enum FeeError {
    #[error("Internal Server Error: {0}")]
    InternalError(String),

    #[error("Bad Request: {0}")]
    BadRequest(String),
}

impl ResponseError for FeeError {
    fn error_response(&self) -> HttpResponse {
        match self {
            FeeError::InternalError(message) => {
                HttpResponse::InternalServerError().body(message.clone())
            }
            FeeError::BadRequest(message) => HttpResponse::BadRequest().body(message.clone()),
        }
    }
}

pub async fn index_html() -> impl Responder {
    let contents = include_str!("index.html");
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(contents)
}

struct FeeHandler {
    state: web::Data<AppState>,
    params: FeeParams,
    config: AlgoConfig,
    seq_fees: SequentialBlockFees,
    last_block_height: u64,
    last_block_time: DateTime<Utc>,
    sma_algo: SmaFeeAlgo<CachingApi<HttpClient>>,
}

impl FeeHandler {
    async fn new(state: web::Data<AppState>, params: FeeParams) -> Result<Self, FeeError> {
        let ending_height = Self::resolve_ending_height(&state, &params).await?;
        let start_height = ending_height.saturating_sub(params.amount_of_blocks);
        let config = Self::parse_config(&params)?;
        let seq_fees = Self::fetch_fees(&state, start_height, ending_height).await?;
        let last_block = Self::get_last_block_info(&state, &seq_fees).await?;
        let sma_algo = SmaFeeAlgo::new(state.fee_api.clone(), config);

        Ok(Self {
            state,
            params,
            config,
            seq_fees,
            last_block_height: last_block.0,
            last_block_time: last_block.1,
            sma_algo,
        })
    }

    async fn get_fees_response(&self) -> Result<FeeResponse, FeeError> {
        let data = self.calculate_fee_data().await?;
        let stats = self.calculate_statistics(&data);
        Ok(FeeResponse { data, stats })
    }

    async fn resolve_ending_height(
        state: &web::Data<AppState>,
        params: &FeeParams,
    ) -> Result<u64, FeeError> {
        if let Some(val) = params.ending_height {
            Ok(val)
        } else {
            state.fee_api.current_height().await.map_err(|e| {
                error!("Error fetching current height: {:?}", e);
                FeeError::InternalError("Failed to fetch current height".into())
            })
        }
    }

    fn parse_config(params: &FeeParams) -> Result<AlgoConfig, FeeError> {
        AlgoConfig::try_from(params.clone()).map_err(|e| {
            error!("Error parsing config: {:?}", e);
            FeeError::BadRequest("Invalid configuration parameters".into())
        })
    }

    async fn fetch_fees(
        state: &web::Data<AppState>,
        start: u64,
        end: u64,
    ) -> Result<SequentialBlockFees, FeeError> {
        state.fee_api.fees(start..=end).await.map_err(|e| {
            error!("Error fetching sequential fees: {:?}", e);
            FeeError::InternalError("Failed to fetch sequential fees".into())
        })
    }

    async fn get_last_block_info(
        state: &web::Data<AppState>,
        seq_fees: &SequentialBlockFees,
    ) -> Result<(u64, DateTime<Utc>), FeeError> {
        let last_block = seq_fees.last();
        let last_block_time = state
            .fee_api
            .inner()
            .get_block_time(last_block.height)
            .await
            .map_err(|e| {
                error!("Error fetching last block time: {:?}", e);
                FeeError::InternalError("Failed to fetch last block time".into())
            })?
            .ok_or_else(|| {
                error!("Last block time not found");
                FeeError::InternalError("Last block time not found".into())
            })?;
        info!("Last block time: {}", last_block_time);
        Ok((last_block.height, last_block_time))
    }

    async fn calculate_fee_data(&self) -> Result<Vec<FeeDataPoint>, FeeError> {
        let mut data = Vec::with_capacity(self.seq_fees.len());

        for block_fee in self.seq_fees.iter() {
            let fee_data = self.process_block_fee(block_fee).await?;
            data.push(fee_data);
        }

        Ok(data)
    }

    async fn process_block_fee(&self, block_fee: &FeesAtHeight) -> Result<FeeDataPoint, FeeError> {
        let current_fee_wei = calculate_blob_tx_fee(self.params.num_blobs, &block_fee.fees);
        let short_fee_wei = self
            .fetch_fee(block_fee.height, self.config.sma_periods.short)
            .await?;
        let long_fee_wei = self
            .fetch_fee(block_fee.height, self.config.sma_periods.long)
            .await?;

        let acceptable = self
            .sma_algo
            .fees_acceptable(
                self.params.num_blobs,
                self.params.num_l2_blocks_behind,
                block_fee.height,
            )
            .await
            .map_err(|e| {
                error!("Error determining fee acceptability: {:?}", e);
                FeeError::InternalError("Failed to determine fee acceptability".into())
            })?;

        let convert = |wei| format!("{:.4}", (wei as f64) / 1e18);

        Ok(FeeDataPoint {
            block_height: block_fee.height,
            current_fee: convert(current_fee_wei),
            short_fee: convert(short_fee_wei),
            long_fee: convert(long_fee_wei),
            acceptable,
        })
    }

    async fn fetch_fee(
        &self,
        current_height: u64,
        period: std::num::NonZeroU64,
    ) -> Result<u128, FeeError> {
        let fees = self
            .state
            .fee_api
            .fees(last_n_blocks(current_height, period))
            .await
            .map_err(|e| {
                error!("Error fetching fees for period: {:?}", e);
                FeeError::InternalError("Failed to fetch fees".into())
            })?
            .mean();
        Ok(calculate_blob_tx_fee(self.params.num_blobs, &fees))
    }

    fn calculate_statistics(&self, data: &[FeeDataPoint]) -> FeeStats {
        let total_blocks = data.len() as f64;
        let acceptable_blocks = data.iter().filter(|d| d.acceptable).count() as f64;
        let percentage_acceptable = if total_blocks > 0.0 {
            (acceptable_blocks / total_blocks) * 100.0
        } else {
            0.0
        };

        let gap_sizes = self.compute_gap_sizes(data);
        let percentile_95_gap_size = Self::calculate_percentile(&gap_sizes, 0.95);
        let longest_unacceptable_streak = gap_sizes.into_iter().max().unwrap_or(0);

        FeeStats {
            percentage_acceptable,
            percentile_95_gap_size,
            longest_unacceptable_streak,
        }
    }

    fn compute_gap_sizes(&self, data: &[FeeDataPoint]) -> Vec<u64> {
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

        if current_gap > 0 {
            gap_sizes.push(current_gap);
        }

        gap_sizes
    }

    fn calculate_percentile(gaps: &[u64], percentile: f64) -> u64 {
        if gaps.is_empty() {
            return 0;
        }

        let mut sorted_gaps = gaps.to_vec();
        sorted_gaps.sort_unstable();

        let index = ((sorted_gaps.len() as f64) * percentile).ceil() as usize - 1;
        sorted_gaps[index.min(sorted_gaps.len() - 1)]
    }
}

pub async fn get_fees(state: web::Data<AppState>, params: web::Query<FeeParams>) -> impl Responder {
    let handler = match FeeHandler::new(state.clone(), params.into_inner()).await {
        Ok(h) => h,
        Err(e) => return e.error_response(),
    };

    match handler.get_fees_response().await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(e) => e.error_response(),
    }
}

pub async fn get_block_time_info(state: web::Data<AppState>) -> impl Responder {
    // Get the current height from the fee API.
    let current_height = match state.fee_api.current_height().await {
        Ok(height) => height,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .body(format!("Error fetching current height: {:?}", e));
        }
    };

    // Get the block time for the current height.
    let last_block_time = match state.fee_api.inner().get_block_time(current_height).await {
        Ok(Some(time)) => time,
        Ok(None) => {
            return HttpResponse::InternalServerError()
                .body("Last block time not found".to_string());
        }
        Err(e) => {
            return HttpResponse::InternalServerError()
                .body(format!("Error fetching block time: {:?}", e));
        }
    };

    // Here we assume a constant block interval (e.g., 12 seconds)
    let block_interval: u64 = 12;

    HttpResponse::Ok().json(json!({
        "last_block_height": current_height,
        "last_block_time": last_block_time.to_rfc3339(),
        "block_interval": block_interval
    }))
}

pub async fn simulate_fees(
    state: web::Data<AppState>,
    params: web::Json<SimulationParams>,
) -> impl Responder {
    let ending_height = FeeHandler::resolve_ending_height(&state, &params.fee_params)
        .await
        .unwrap();

    let start_height = ending_height.saturating_sub(params.fee_params.amount_of_blocks);
    let fees = FeeHandler::fetch_fees(&state, start_height, ending_height)
        .await
        .unwrap()
        .into_iter()
        .collect_vec();

    let config = FeeHandler::parse_config(&params.fee_params).unwrap();
    let sma_algo = SmaFeeAlgo::new(state.fee_api.clone(), config);

    let sim_result = run_simulation(
        &fees,
        params.bundling_interval_blocks,
        params.bundle_blob_count,
        params.finalization_time_minutes, // <-- pass here
        &sma_algo,
    )
    .await;

    HttpResponse::Ok().json(sim_result)
}

/// Looks up the fee entry corresponding to a given block height.
/// Assumes fee_history is sorted by block_height.
fn fees_at_height(fee_history: &[FeesAtHeight], current_height: u64) -> (&FeesAtHeight, u64) {
    for entry in fee_history {
        if entry.height >= current_height {
            return (entry, entry.height);
        }
    }
    let last = fee_history.last().unwrap();
    (last, last.height)
}

/// Runs the simulation by stepping through block heights.
/// At each step, it adds a full bundle (bundle_blob_count blobs) to the backlog,
/// computes L2 blocks behind as:
///
///     l2_blocks_behind = backlog × (bundling_interval_blocks / bundle_blob_count)
///
/// Then, it attempts to commit up to 6 blobs (commit_blob_count = min(backlog, 6)) at that step.
/// For each commit, it uses the precise helper calculate_blob_tx_fee with the actual commit_blob_count
/// and the current fee structure. The fee algorithm is called with commit_blob_count, the computed
/// l2_blocks_behind, and the current block height.
pub async fn run_simulation(
    fee_history: &[FeesAtHeight],
    bundling_interval_blocks: u32,
    bundle_blob_count: u32,
    finalization_time_minutes: u32,
    fee_algo: &SmaFeeAlgo<CachingApi<HttpClient>>,
) -> SimulationResult {
    if fee_history.is_empty() {
        return SimulationResult {
            immediate_total_fee: 0.0,
            algorithm_total_fee: 0.0,
            eth_saved: 0.0,
            timeline: vec![],
        };
    }

    let finalization_time_seconds = (finalization_time_minutes as u64) * 60;

    let mut immediate_total_fee: u128 = 0;
    let mut algorithm_total_fee: u128 = 0;

    // TIME: each L1 block is ~12s
    let mut current_time: u64 = 0;

    // "Immediate" approach
    let mut immediate_l2_blocks_acc = 0;
    let mut immediate_backlog_blobs = 0;
    let mut last_commit_time_immediate: u64 = 0;

    // "Algorithm" approach
    let mut algo_l2_blocks_acc = 0;
    let mut algo_backlog_blobs = 0;
    let mut last_commit_time_algo: u64 = 0;

    let mut timeline = Vec::with_capacity(fee_history.len());

    for entry in fee_history {
        // One L1 block => 12 seconds
        current_time += 12;

        // ====================================
        // 1) Immediate Approach
        // ====================================
        immediate_l2_blocks_acc += 12;
        while immediate_l2_blocks_acc >= bundling_interval_blocks {
            immediate_backlog_blobs += bundle_blob_count;
            immediate_l2_blocks_acc -= bundling_interval_blocks;
        }

        if immediate_backlog_blobs > 0 {
            let dt = current_time.saturating_sub(last_commit_time_immediate);
            if dt >= finalization_time_seconds {
                // Commit everything
                let fee = calculate_blob_tx_fee(immediate_backlog_blobs, &entry.fees);
                immediate_total_fee = immediate_total_fee.saturating_add(fee);
                immediate_backlog_blobs = 0;
                last_commit_time_immediate = current_time;
            }
        }

        // How many L2 blocks behind for "immediate" path?
        // For each `bundle_blob_count` backlog, that's `bundling_interval_blocks` L2 blocks not committed.
        let immediate_l2_behind = immediate_backlog_blobs
            .saturating_mul(bundling_interval_blocks / bundle_blob_count.max(1));

        // ====================================
        // 2) Algorithm Approach
        // ====================================
        algo_l2_blocks_acc += 12;
        while algo_l2_blocks_acc >= bundling_interval_blocks {
            algo_backlog_blobs += bundle_blob_count;
            algo_l2_blocks_acc -= bundling_interval_blocks;
        }

        // Compute how many L2 blocks behind for the algo approach
        let algo_l2_behind =
            algo_backlog_blobs.saturating_mul(bundling_interval_blocks / bundle_blob_count.max(1));

        // Possibly commit up to 6
        let commit_blob_count = algo_backlog_blobs.min(6);
        if commit_blob_count > 0 {
            let fee = calculate_blob_tx_fee(commit_blob_count, &entry.fees);
            let acceptable = fee_algo
                .fees_acceptable(commit_blob_count, algo_l2_behind, entry.height)
                .await
                .unwrap_or(false);

            if acceptable {
                let dt = current_time.saturating_sub(last_commit_time_algo);
                if dt >= finalization_time_seconds {
                    // Commit up to 6
                    algorithm_total_fee = algorithm_total_fee.saturating_add(fee);
                    algo_backlog_blobs -= commit_blob_count;
                    last_commit_time_algo = current_time;
                }
            }
        }

        // Recompute after possibly committing
        let algo_l2_behind_after =
            algo_backlog_blobs.saturating_mul(bundling_interval_blocks / bundle_blob_count.max(1));

        // ====================================
        // 3) Record timeline point
        // ====================================
        timeline.push(SimulationPoint {
            block_height: entry.height,
            immediate_fee: immediate_total_fee as f64 / 1e18,
            algorithm_fee: algorithm_total_fee as f64 / 1e18,
            immediate_l2_behind,
            // Use the updated behind if you like:
            algo_l2_behind: algo_l2_behind_after,
        });
    }

    // ====================================
    // 4) Leftover backlog for algo
    // ====================================
    if algo_backlog_blobs > 0 {
        let last = fee_history.last().unwrap();
        let fee = calculate_blob_tx_fee(algo_backlog_blobs, &last.fees);
        algorithm_total_fee = algorithm_total_fee.saturating_add(fee);

        timeline.push(SimulationPoint {
            block_height: last.height,
            immediate_fee: immediate_total_fee as f64 / 1e18,
            algorithm_fee: algorithm_total_fee as f64 / 1e18,
            immediate_l2_behind: immediate_backlog_blobs
                .saturating_mul(bundling_interval_blocks / bundle_blob_count.max(1)),
            algo_l2_behind: 0,
        });
        algo_backlog_blobs = 0;
    }

    let eth_saved = (immediate_total_fee.saturating_sub(algorithm_total_fee)) as f64 / 1e18;
    SimulationResult {
        immediate_total_fee: immediate_total_fee as f64 / 1e18,
        algorithm_total_fee: algorithm_total_fee as f64 / 1e18,
        eth_saved,
        timeline,
    }
}
