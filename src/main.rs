use std::sync::Arc;

use actix_web::{dev::Url, http::Uri};
use adapters::storage::InMemoryStorage;
use api::launch;
use prometheus::Registry;
use services::StatusReporter;
use setup::{spawn_block_watcher, Config, ExtraConfig};

use crate::errors::Result;

mod adapters;
mod api;
mod cli;
mod common;
mod errors;
mod services;
mod setup;

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

    let status_reporter = Arc::new(StatusReporter::new(storage.clone()));
    launch(Arc::clone(&metrics_registry), status_reporter).await?;

    Ok(())
}
