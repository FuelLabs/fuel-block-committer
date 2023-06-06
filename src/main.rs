use api::launch_api_server;
use prometheus::Registry;

use errors::Result;
use setup::{
    config::InternalConfig,
    helpers::{
        create_eth_rpc, setup_logger, setup_storage, spawn_block_watcher,
        spawn_eth_committer_and_listener,
    },
};

mod adapters;
mod api;
mod cli;
mod common;
mod errors;
mod services;
mod setup;
mod telemetry;

#[tokio::main]
async fn main() -> Result<()> {
    let config = cli::parse();
    let internal_config = InternalConfig::default();
    let metrics_registry = Registry::default();
    let storage = setup_storage(&config)?;

    setup_logger();

    let (rx_fuel_block, _block_watcher_handle, fuel_health_check) = spawn_block_watcher(
        &config,
        &internal_config,
        storage.clone(),
        &metrics_registry,
    );

    let (ethereum_rpc, eth_health_check) =
        create_eth_rpc(&config, &internal_config, &metrics_registry).await?;

    let (_committer_handle, _listener_handle) = spawn_eth_committer_and_listener(
        &config,
        &internal_config,
        rx_fuel_block,
        ethereum_rpc,
        storage.clone(),
        &metrics_registry,
    )?;

    launch_api_server(
        &config,
        metrics_registry,
        storage,
        fuel_health_check,
        eth_health_check,
    )
    .await?;

    Ok(())
}
