use std::sync::Arc;


use adapters::storage::InMemoryStorage;
use api::launch_api_server;
use prometheus::Registry;
use services::{HealthReporter, StatusReporter};
use setup::{spawn_block_watcher, Config, ExtraConfig};

use crate::errors::Result;

mod adapters;
mod api;
mod cli;
mod common;
mod errors;
mod health_check;
mod metrics;
mod services;
mod setup;

#[tokio::main]
async fn main() -> Result<()> {
    // todo: get config from cli
    let config = Config::default();
    let extra_config = ExtraConfig::default();

    let storage = InMemoryStorage::new();

    let metrics_registry = Arc::new(Registry::default());

    let (_rx_fuel_block, _block_watcher_handle, fuel_health_check) =
        spawn_block_watcher(&config, &extra_config, storage.clone(), &metrics_registry).await?;

    let status_reporter = Arc::new(StatusReporter::new(storage.clone()));
    let health_reporter = Arc::new(HealthReporter::new(fuel_health_check));
    launch_api_server(
        Arc::clone(&metrics_registry),
        status_reporter,
        health_reporter,
    )
    .await?;

    Ok(())
}
