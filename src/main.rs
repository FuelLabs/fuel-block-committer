use adapters::storage::InMemoryStorage;
use api::launch_api_server;
use prometheus::Registry;
use setup::{
    config::{Config, ExtraConfig},
    helpers::spawn_block_watcher,
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
    tracing_subscriber::fmt::init();

    let extra_config = ExtraConfig::default();

    let storage = InMemoryStorage::new();

    let metrics_registry = Registry::default();

    let (_rx_fuel_block, _block_watcher_handle, fuel_health_check) =
        spawn_block_watcher(&config, &extra_config, storage.clone(), &metrics_registry)?;

    launch_api_server(metrics_registry, storage, fuel_health_check).await?;

    Ok(())
}
