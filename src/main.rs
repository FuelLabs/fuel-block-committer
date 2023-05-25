use std::sync::{Arc, Mutex};

use actix_web::{web, App, HttpServer};
use adapters::storage::InMemoryStorage;
use serde::Serialize;
use setup::{spawn_block_watcher, Config, ExtraConfig};

use crate::errors::Result;

mod adapters;
mod api;
mod cli;
mod common;
mod errors;
mod services;
mod setup;

use services::{BlockCommitter, CommitListener, MetricsService};

// todo/note: each of these fields could be separately hidden behind a mutex
// so that we don't have to lock the whole struct - depending on the usecases
pub type AppState = Arc<Mutex<StatusReport>>;

#[tokio::main]
async fn main() -> Result<()> {
    // todo: get config from cli
    let config = Config::default();
    let extra_config = ExtraConfig::default();

    // AppState actix::web
    let app_state = Arc::new(Mutex::new(StatusReport::default()));
    let (tx_fuel_block, rx_fuel_block) = tokio::sync::mpsc::channel(100);

    let storage = InMemoryStorage::new();

    let _block_watcher_handle =
        spawn_block_watcher(&config, &extra_config, storage.clone(), tx_fuel_block).await?;

    // service BlockCommitter
    let ethereum_rpc = config.ethereum_rpc.clone();
    tokio::spawn(async move {
        let mut block_committer = BlockCommitter::new(rx_fuel_block, ethereum_rpc);
        block_committer.run().await.unwrap();
    });

    // service CommitListener
    let commit_listener = CommitListener::new(
        config.ethereum_rpc.clone(),
        config.state_contract_address,
        app_state.clone(),
    );

    // run the service
    tokio::spawn(async move {
        commit_listener.run().await.unwrap();
    });

    // Database

    // prometheus
    let _metrics = MetricsService;

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

    Ok(())
}

#[derive(Serialize, Debug, Default)]
pub enum Status {
    #[default]
    Idle,
    Commiting,
}

#[derive(Debug, Serialize, Default)]
pub struct StatusReport {
    pub latest_fuel_block: u64,
    pub latest_committed_block: u64,
    pub status: Status,
    pub ethereum_wallet_gas_balance: u64,
}
