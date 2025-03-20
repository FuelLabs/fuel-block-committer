use std::num::NonZeroU64;

use actix_web::{Responder, ResponseError, web};
use anyhow::Result;
use eth::HttpClient;
use services::{
    fee_metrics_tracker::service::calculate_blob_tx_fee,
    fees::{Api, FeesAtHeight, SequentialBlockFees, cache::CachingApi},
    state_committer::{AlgoConfig, SmaFeeAlgo},
};
use tracing::error;

use crate::{
    handlers::error::FeeError,
    models::{FeeDataPoint, FeeParams, FeeResponse, FeeStats},
    state::AppState,
    utils::{last_n_blocks, wei_to_eth_string},
};

pub struct FeeHandler {
    state: web::Data<AppState>,
    params: FeeParams,
    config: AlgoConfig,
    seq_fees: SequentialBlockFees,
    sma_algo: SmaFeeAlgo<CachingApi<HttpClient>>,
}

impl FeeHandler {
    pub async fn new(state: web::Data<AppState>, params: FeeParams) -> Result<Self, FeeError> {
        let ending_height = Self::resolve_ending_height(&state, &params).await?;
        let start_height = ending_height.saturating_sub(params.amount_of_blocks);

        let config = Self::parse_config(&params)?;
        let seq_fees = Self::fetch_fees(&state, start_height, ending_height).await?;
        let sma_algo = SmaFeeAlgo::new(state.fee_api.clone(), config);

        Ok(Self {
            state,
            params,
            config,
            seq_fees,
            sma_algo,
        })
    }

    pub async fn get_fees_response(&self) -> Result<FeeResponse, FeeError> {
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

    async fn calculate_fee_data(&self) -> Result<Vec<FeeDataPoint>, FeeError> {
        let mut data = Vec::with_capacity(self.seq_fees.len());
        for block_fee in self.seq_fees.iter() {
            let point = self.process_block_fee(block_fee).await?;
            data.push(point);
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

        Ok(FeeDataPoint {
            block_height: block_fee.height,
            current_fee: wei_to_eth_string(current_fee_wei),
            short_fee: wei_to_eth_string(short_fee_wei),
            long_fee: wei_to_eth_string(long_fee_wei),
            acceptable,
        })
    }

    async fn fetch_fee(&self, current_height: u64, period: NonZeroU64) -> Result<u128, FeeError> {
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
        Ok(response) => actix_web::HttpResponse::Ok().json(response),
        Err(e) => e.error_response(),
    }
}
