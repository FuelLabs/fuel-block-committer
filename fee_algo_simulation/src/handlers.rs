use std::time::Duration;

use actix_web::{web, HttpResponse, Responder, ResponseError};
use anyhow::Result;
use eth::HttpClient;
use services::{
    fee_metrics_tracker::service::calculate_blob_tx_fee,
    fees::{cache::CachingApi, Api, FeesAtHeight, SequentialBlockFees},
    state_committer::{AlgoConfig, SmaFeeAlgo},
    types::{DateTime, Utc},
};
use thiserror::Error;
use tracing::{error, info};

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

        let block_gap = self.last_block_height - block_fee.height;
        let block_time = self.last_block_time - Duration::from_secs(12 * block_gap);

        let convert = |wei| format!("{:.4}", (wei as f64) / 1e18);

        Ok(FeeDataPoint {
            block_height: block_fee.height,
            block_time: block_time.to_rfc3339(),
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
