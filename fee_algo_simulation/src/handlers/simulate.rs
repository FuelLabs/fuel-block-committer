use actix_web::{Responder, ResponseError, web};
use anyhow::Result;
use eth::HttpClient;
use itertools::Itertools;
use services::{
    fee_metrics_tracker::service::calculate_blob_tx_fee,
    fees::{Api, Fees, FeesAtHeight, cache::CachingApi},
    state_committer::{AlgoConfig, SmaFeeAlgo},
};
use tracing::error;

use crate::{handlers::error::FeeError, utils::ETH_BLOCK_TIME}; // Our custom error
use crate::{
    models::{SimulationParams, SimulationPoint, SimulationResult},
    state::AppState,
};

struct ImmediateSim {
    l2_behind: u32,
    total_fee_wei: u128,
    last_commit_time: u64,
    finalization_time_seconds: u64,
    bundle_blob_count: u32,
    bundling_interval_blocks: u32,
}

impl ImmediateSim {
    fn new(
        finalization_time_seconds: u64,
        bundle_blob_count: u32,
        bundling_interval_blocks: u32,
    ) -> Self {
        Self {
            l2_behind: 0,
            total_fee_wei: 0,
            last_commit_time: 0,
            finalization_time_seconds,
            bundle_blob_count,
            bundling_interval_blocks,
        }
    }

    fn on_l1_block(&mut self, block_fees: &Fees, current_time: u64, newly_produced_l2: u32) {
        self.l2_behind += newly_produced_l2;
        self.maybe_commit_all(block_fees, current_time);
    }

    fn leftover_commit(&mut self, block_fees: &Fees, current_time: u64) {
        self.maybe_commit_all(block_fees, current_time);
    }

    fn maybe_commit_all(&mut self, block_fees: &Fees, current_time: u64) {
        let dt = current_time.saturating_sub(self.last_commit_time);
        if dt < self.finalization_time_seconds {
            return;
        }

        let bundles = self.l2_behind / self.bundling_interval_blocks;
        if bundles > 0 {
            let total_blobs = bundles * self.bundle_blob_count;
            let fee = calculate_blob_tx_fee(total_blobs, block_fees);
            self.total_fee_wei = self.total_fee_wei.saturating_add(fee);

            self.l2_behind -= bundles * self.bundling_interval_blocks;
            self.last_commit_time = current_time;
        }
    }

    fn total_fee_wei(&self) -> u128 {
        self.total_fee_wei
    }

    fn l2_behind(&self) -> u32 {
        self.l2_behind
    }
}

struct AlgorithmSim {
    l2_behind: u32,
    total_fee_wei: u128,
    last_commit_time: u64,
    finalization_time_seconds: u64,
    bundle_blob_count: u32,
    bundling_interval_blocks: u32,
    fee_algo: SmaFeeAlgo<CachingApi<HttpClient>>,
    max_bundles_per_commit: u32,
}

impl AlgorithmSim {
    fn new(
        finalization_time_seconds: u64,
        bundle_blob_count: u32,
        bundling_interval_blocks: u32,
        fee_algo: SmaFeeAlgo<CachingApi<HttpClient>>,
        max_bundles_per_commit: u32,
    ) -> Self {
        Self {
            l2_behind: 0,
            total_fee_wei: 0,
            last_commit_time: 0,
            finalization_time_seconds,
            bundle_blob_count,
            bundling_interval_blocks,
            fee_algo,
            max_bundles_per_commit,
        }
    }

    async fn on_l1_block(
        &mut self,
        block_fees: &Fees,
        block_height: u64,
        current_time: u64,
        newly_produced_l2: u32,
    ) -> Result<(), FeeError> {
        self.l2_behind += newly_produced_l2;
        self.maybe_commit_partial(block_fees, block_height, current_time)
            .await?;
        Ok(())
    }

    async fn leftover_commit(
        &mut self,
        block_fees: &Fees,
        block_height: u64,
        current_time: u64,
    ) -> Result<(), FeeError> {
        self.maybe_commit_partial(block_fees, block_height, current_time)
            .await
    }

    async fn maybe_commit_partial(
        &mut self,
        block_fees: &Fees,
        block_height: u64,
        current_time: u64,
    ) -> Result<(), FeeError> {
        let dt = current_time.saturating_sub(self.last_commit_time);
        if dt < self.finalization_time_seconds {
            return Ok(());
        }

        let total_bundles = self.l2_behind / self.bundling_interval_blocks;
        if total_bundles == 0 {
            return Ok(());
        }

        let bundles_to_commit = total_bundles.min(self.max_bundles_per_commit);
        let commit_blob_count = bundles_to_commit * self.bundle_blob_count;

        let fee_wei = calculate_blob_tx_fee(commit_blob_count, block_fees);
        let acceptable = self
            .fee_algo
            .fees_acceptable(commit_blob_count, self.l2_behind(), block_height)
            .await
            .map_err(|_| FeeError::InternalError("Error checking fee acceptability".into()))?;

        if acceptable {
            self.total_fee_wei = self.total_fee_wei.saturating_add(fee_wei);
            self.l2_behind -= bundles_to_commit * self.bundling_interval_blocks;
            self.last_commit_time = current_time;
        }

        Ok(())
    }

    fn total_fee_wei(&self) -> u128 {
        self.total_fee_wei
    }

    fn l2_behind(&self) -> u32 {
        self.l2_behind
    }
}

struct SimulateHandler {
    fee_history: Vec<FeesAtHeight>,
    immediate: ImmediateSim,
    algorithm: AlgorithmSim,
}

impl SimulateHandler {
    fn new(
        params: SimulationParams,
        fee_history: Vec<FeesAtHeight>,
        fee_algo: SmaFeeAlgo<CachingApi<HttpClient>>,
    ) -> Self {
        let finalization_time_seconds = (params.finalization_time_minutes as u64) * 60;

        let immediate = ImmediateSim::new(
            finalization_time_seconds,
            params.bundle_blob_count,
            params.bundling_interval_blocks,
        );

        let algorithm = AlgorithmSim::new(
            finalization_time_seconds,
            params.bundle_blob_count,
            params.bundling_interval_blocks,
            fee_algo,
            6, // e.g. up to 6 bundles per commit
        );

        Self {
            fee_history,
            immediate,
            algorithm,
        }
    }

    async fn run_simulation(mut self) -> Result<SimulationResult, FeeError> {
        if self.fee_history.is_empty() {
            return Ok(SimulationResult {
                immediate_total_fee: 0.0,
                algorithm_total_fee: 0.0,
                eth_saved: 0.0,
                timeline: vec![],
            });
        }

        let mut timeline = Vec::new();
        let mut current_time = 0u64;

        for entry in &self.fee_history {
            current_time += ETH_BLOCK_TIME;

            self.immediate.on_l1_block(&entry.fees, current_time, 12);

            self.algorithm
                .on_l1_block(&entry.fees, entry.height, current_time, 12)
                .await?;

            timeline.push(SimulationPoint {
                block_height: entry.height,
                immediate_fee: self.immediate.total_fee_wei() as f64 / 1e18,
                algorithm_fee: self.algorithm.total_fee_wei() as f64 / 1e18,
                immediate_l2_behind: self.immediate.l2_behind(),
                algo_l2_behind: self.algorithm.l2_behind(),
            });
        }

        let last_block_fees = &self.fee_history.last().unwrap().fees;
        let last_block_height = self.fee_history.last().unwrap().height;

        while self.immediate.l2_behind() > 0 || self.algorithm.l2_behind() > 0 {
            current_time += ETH_BLOCK_TIME;

            self.immediate
                .leftover_commit(last_block_fees, current_time);
            self.algorithm
                .leftover_commit(last_block_fees, last_block_height, current_time)
                .await?;

            timeline.push(SimulationPoint {
                block_height: last_block_height,
                immediate_fee: self.immediate.total_fee_wei() as f64 / 1e18,
                algorithm_fee: self.algorithm.total_fee_wei() as f64 / 1e18,
                immediate_l2_behind: self.immediate.l2_behind(),
                algo_l2_behind: self.algorithm.l2_behind(),
            });
        }

        let immediate_fee = self.immediate.total_fee_wei();
        let algorithm_fee = self.algorithm.total_fee_wei();
        let eth_saved = (immediate_fee.saturating_sub(algorithm_fee)) as f64 / 1e18;

        Ok(SimulationResult {
            immediate_total_fee: immediate_fee as f64 / 1e18,
            algorithm_total_fee: algorithm_fee as f64 / 1e18,
            eth_saved,
            timeline,
        })
    }
}

pub async fn simulate_fees(
    state: web::Data<AppState>,
    params: web::Json<SimulationParams>,
) -> impl Responder {
    let ending_height = match params.fee_params.ending_height {
        Some(h) => h,
        None => match state.fee_api.current_height().await {
            Ok(h2) => h2,
            Err(e) => {
                error!("Error fetching current height: {:?}", e);
                return FeeError::InternalError("Could not fetch current height".into())
                    .error_response();
            }
        },
    };
    let start_height = ending_height.saturating_sub(params.fee_params.amount_of_blocks);

    let config = match AlgoConfig::try_from(params.fee_params.clone()) {
        Ok(c) => c,
        Err(e) => {
            error!("Error parsing config: {:?}", e);
            return FeeError::BadRequest("Invalid configuration".into()).error_response();
        }
    };

    let fee_algo = SmaFeeAlgo::new(state.fee_api.clone(), config);

    let block_fees = match state.fee_api.fees(start_height..=ending_height).await {
        Ok(seq) => seq.into_iter().collect_vec(),
        Err(e) => {
            error!("Error fetching fees for simulate: {:?}", e);
            return FeeError::InternalError("Failed to fetch fees".to_string()).error_response();
        }
    };

    let sim_handler = SimulateHandler::new(params.into_inner(), block_fees, fee_algo);
    let sim_result = match sim_handler.run_simulation().await {
        Ok(r) => r,
        Err(e) => return e.error_response(),
    };

    actix_web::HttpResponse::Ok().json(sim_result)
}
