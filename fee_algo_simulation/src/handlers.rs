use actix_web::HttpResponse; // Usually you want these top-level imports
use serde_json::json;
use tracing::error;

// Bring in your crate's modules, etc.
use crate::{
    models::{
        FeeDataPoint, FeeParams, FeeResponse, FeeStats, SimulationParams, SimulationPoint,
        SimulationResult,
    },
    state::AppState,
    utils::{last_n_blocks, wei_to_eth_string},
};

// Services from your library
use actix_web::ResponseError;
use anyhow::Result;
use eth::HttpClient;
use itertools::Itertools;
use services::{
    fee_metrics_tracker::service::calculate_blob_tx_fee,
    fees::{Api, Fees, FeesAtHeight, SequentialBlockFees, cache::CachingApi},
    state_committer::{AlgoConfig, SmaFeeAlgo},
};
use std::num::NonZeroU64;
pub mod block_time_info;
pub mod error;
pub mod fee;
pub mod index;
pub mod simulate;
