use adapters::storage::InMemoryStorage;
use api::launch_api_server;
use prometheus::Registry;
use setup::{config::InternalConfig, helpers::{spawn_block_watcher, setup_logger}};

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

    let (_rx_fuel_block, _block_watcher_handle, fuel_health_check) =
        spawn_block_watcher(&config, &internal_config, storage.clone(), &metrics_registry)?;

    launch_api_server(metrics_registry, storage, fuel_health_check).await?;

    Ok(())
}
