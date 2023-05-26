use std::sync::Arc;

use adapters::storage::InMemoryStorage;
use api::launch_api_server;
use prometheus::Registry;
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

    let metrics_registry = Registry::default();

    let (_rx_fuel_block, _block_watcher_handle, fuel_health_check) =
        spawn_block_watcher(&config, &extra_config, storage.clone(), &metrics_registry)?;

    launch_api_server(Arc::new(metrics_registry), storage, fuel_health_check).await?;

    Ok(())
}
