use adapters::storage::InMemoryStorage;
use api::launch_api_server;
use prometheus::Registry;

use setup::{
    config::InternalConfig,
    helpers::{setup_logger, spawn_eth_committer_listener, spawn_fake_block_watcher},
};

use crate::errors::Result;

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
    let config = cli::parse()?;
    let internal_config = InternalConfig::default();

    setup_logger();

    let storage = InMemoryStorage::new();

    let metrics_registry = Registry::default();

    // let (rx_fuel_block, _block_watcher_handle, fuel_health_check) = spawn_block_watcher(
    //     &config,
    //     &internal_config,
    //     storage.clone(),
    //     &metrics_registry,
    // )?;

    let (rx_fuel_block, _block_watcher_handle, fuel_health_check) = spawn_fake_block_watcher(
        &config,
        &internal_config,
        storage.clone(),
        &metrics_registry,
    )?;

    let (_committer_handle, _listener_handle, eth_health_check) = spawn_eth_committer_listener(
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
