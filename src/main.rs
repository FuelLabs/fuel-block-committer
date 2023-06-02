use api::launch_api_server;
use prometheus::Registry;

use errors::Result;
use setup::{
    config::InternalConfig,
    helpers::{setup_logger, setup_storage, spawn_block_watcher, spawn_eth_committer_and_listener},
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

    let (_committer_handle, _listener_handle, eth_health_check) = spawn_eth_committer_and_listener(
        &config,
        &internal_config,
        rx_fuel_block,
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
