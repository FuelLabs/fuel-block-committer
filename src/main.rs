use std::{
    sync::{Arc, Mutex},
    vec,
};

use actix_web::{dev::Url, get, http::Uri, web, App, HttpServer};
use adapters::storage::InMemoryStorage;
use prometheus::{Encoder, Registry, TextEncoder};
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



// todo/note: each of these fields could be separately hidden behind a mutex
// so that we don't have to lock the whole struct - depending on the usecases
pub type AppState = Arc<Mutex<StatusReport>>;

#[get("/metrics")]
async fn metrics(registry: web::Data<Arc<Registry>>) -> String {
    let encoder = TextEncoder::new();

    let mut buf: Vec<u8> = vec![];
    let metrics = registry.gather();

    encoder.encode(&metrics, &mut buf).unwrap();

    let metrics = prometheus::gather();
    encoder.encode(&metrics, &mut buf).unwrap();

    String::from_utf8(buf).unwrap()
}

#[tokio::main]
async fn main() -> Result<()> {
    // todo: get config from cli
    let mut config = Config::default();
    config.fuel_graphql_endpoint = Url::new(
        Uri::builder()
            .path_and_query("localhost:18930")
            .build()
            .unwrap(),
    );
    let extra_config = ExtraConfig::default();

    // AppState actix::web
    let app_state = Arc::new(Mutex::new(StatusReport::default()));

    let storage = InMemoryStorage::new();

    let metrics_registry = Arc::new(Registry::default());

    let (_rx_fuel_block, _block_watcher_handle) =
        spawn_block_watcher(&config, &extra_config, storage.clone(), &metrics_registry).await?;

    // // service BlockCommitter
    // let ethereum_rpc = config.ethereum_rpc.clone();
    // tokio::spawn(async move {
    //     let mut block_committer = BlockCommitter::new(rx_fuel_block, ethereum_rpc);
    //     block_committer.run().await.unwrap();
    // });
    //
    // // service CommitListener
    // let commit_listener = CommitListener::new(
    //     config.ethereum_rpc.clone(),
    //     config.state_contract_address,
    //     app_state.clone(),
    // );
    //
    // // run the service
    // tokio::spawn(async move {
    //     commit_listener.run().await.unwrap();
    // });

    let _ = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(app_state.clone()))
            .app_data(web::Data::new(Arc::clone(&metrics_registry)))
            // .service(health)
            // .service(status)
            .service(metrics)
    })
    .bind(("127.0.0.1", 7070))
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
