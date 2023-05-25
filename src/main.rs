use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use actix_web::{dev::Url, web, App, HttpServer};
use adapters::block_fetcher::{self, BlockFetcher, FakeBlockFetcher};
use api::launch;
use fuels::{accounts::fuel_crypto::fuel_types::Bytes20, client::schema::Bytes32};
use serde::Serialize;

mod actors;
mod adapters;
mod api;
mod cli;
mod common;
mod errors;
mod services;

use services::{BlockCommitter, BlockWatcher, CommitListener, MetricsService};

#[derive(Default, Debug, Clone)]
pub struct Config {
    pub ethereum_wallet_key: Bytes32,
    pub ethereum_rpc: Url,
    pub fuel_graphql_endpoint: Url,
    pub state_contract_address: Bytes20,
    pub commit_interval: u32,
}

// todo/note: each of these fields could be separately hidden behind a mutex
// so that we don't have to lock the whole struct - depending on the usecases
pub type AppState = Arc<Mutex<StatusReport>>;

#[tokio::main]
async fn main() {
    // todo: get config from cli
    let config = Config::default();

    // AppState actix::web
    let app_state = Arc::new(Mutex::new(StatusReport::default()));
    let (tx_fuel_block, rx_fuel_block) = tokio::sync::mpsc::channel(100);

    // service BlockWatcher
    tokio::spawn(async move {
        let block_fetcher = FakeBlockFetcher {};
        let block_watcher =
            BlockWatcher::new(Duration::from_secs(30), tx_fuel_block, block_fetcher);

        // todo: make fetcher thread safe before running
        // block_watcher.run().await.unwrap();
    });

    // service BlockCommitter
    let ethereum_rpc = config.ethereum_rpc.clone();
    tokio::spawn(async move {
        let mut block_committer = BlockCommitter::new(rx_fuel_block, ethereum_rpc);
        block_committer.run().await.unwrap();
    });

    // service CommitListener
    let commit_listener = CommitListener::new(
        config.ethereum_rpc.clone(),
        config.state_contract_address.clone(),
        app_state.clone(),
    );

    // run the service
    tokio::spawn(async move {
        commit_listener.run().await.unwrap();
    });

    // Database

    // prometheus
    let metrics = MetricsService;

    let _ = HttpServer::new(move || {
        App::new().app_data(web::Data::new(app_state.clone()))
        // .service(health)
        // .service(status)
        // .service(metrics)
    })
    .bind(("127.0.0.1", 8080))
    .unwrap() //TODO read via config PARAM
    .run()
    .await;
}

#[derive(Serialize, Debug)]
pub enum Status {
    Idle,
    Commiting,
}

impl Default for Status {
    fn default() -> Self {
        Status::Idle
    }
}

#[derive(Debug, Serialize, Default)]
pub struct StatusReport {
    pub latest_fuel_block: u64,
    pub latest_committed_block: u64,
    pub status: Status,
    pub ethereum_wallet_gas_balance: u64,
}
