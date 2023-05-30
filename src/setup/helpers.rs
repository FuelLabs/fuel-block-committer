use std::time::Duration;

use fuels::types::block::Block as FuelBlock;
use prometheus::Registry;
use tokio::sync::mpsc::Receiver;
use tracing::error;

use crate::{
    adapters::{
        block_fetcher::{fake_block_fetcher::FakeBlockFetcher, FuelBlockFetcher},
        runner::Runner,
        storage::InMemoryStorage,
    },
    errors::Result,
    services::BlockWatcher,
    setup::config::{Config, InternalConfig},
    telemetry::{ConnectionHealthTracker, HealthChecker, RegistersMetrics},
};

pub fn spawn_fake_block_watcher(
    config: &Config,
    internal_config: &InternalConfig,
    storage: InMemoryStorage,
    registry: &Registry,
) -> Result<(
    Receiver<FuelBlock>,
    tokio::task::JoinHandle<()>,
    HealthChecker,
)> {
    let block_fetcher = FakeBlockFetcher {};

    let (tx_fuel_block, rx_fuel_block) = tokio::sync::mpsc::channel(100);
    let block_watcher =
        BlockWatcher::new(config.commit_epoch, tx_fuel_block, block_fetcher, storage);
    block_watcher.register_metrics(registry);

    let handle = schedule_polling(internal_config.fuel_polling_interval, block_watcher);

    Ok((
        rx_fuel_block,
        handle,
        Box::new(ConnectionHealthTracker::new(0)),
    ))
}

pub fn spawn_block_watcher(
    config: &Config,
    internal_config: &InternalConfig,
    storage: InMemoryStorage,
    registry: &Registry,
) -> Result<(
    Receiver<FuelBlock>,
    tokio::task::JoinHandle<()>,
    HealthChecker,
)> {
    let (block_fetcher, fuel_connection_health) =
        create_block_fetcher(config, internal_config, registry);

    let (block_watcher, rx) = create_block_watcher(config, registry, block_fetcher, storage);

    let handle = schedule_polling(internal_config.fuel_polling_interval, block_watcher);

    Ok((rx, handle, fuel_connection_health))
}

pub fn schedule_polling(
    polling_interval: Duration,
    mut runner: impl Runner + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(e) = runner.run().await {
                error!("Block watcher encountered an error: {e}");
            }
            tokio::time::sleep(polling_interval).await;
        }
    })
}

fn create_block_fetcher(
    config: &Config,
    internal_config: &InternalConfig,
    registry: &Registry,
) -> (FuelBlockFetcher, HealthChecker) {
    let block_fetcher = FuelBlockFetcher::new(
        &config.fuel_graphql_endpoint,
        internal_config.fuel_errors_before_unhealthy,
    );
    block_fetcher.register_metrics(registry);

    let fuel_connection_health = block_fetcher.connection_health_checker();

    (block_fetcher, fuel_connection_health)
}

fn create_block_watcher(
    config: &Config,
    registry: &Registry,
    block_fetcher: FuelBlockFetcher,
    storage: InMemoryStorage,
) -> (BlockWatcher, Receiver<FuelBlock>) {
    let (tx_fuel_block, rx_fuel_block) = tokio::sync::mpsc::channel(100);
    let block_watcher =
        BlockWatcher::new(config.commit_epoch, tx_fuel_block, block_fetcher, storage);
    block_watcher.register_metrics(registry);

    (block_watcher, rx_fuel_block)
}

pub fn setup_logger() {
    tracing_subscriber::fmt::init();
}
