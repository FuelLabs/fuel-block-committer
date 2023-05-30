use std::time::Duration;

use fuels::types::block::Block as FuelBlock;
use prometheus::Registry;
use tokio::sync::mpsc::Receiver;
use tracing::error;

use crate::{
    adapters::{
        block_fetcher::{fake_block_fetcher::FakeBlockFetcher, FuelBlockFetcher},
        ethereum_rpc::EthereumRPC,
        runner::Runner,
        storage::InMemoryStorage,
    },
    errors::Result,
    services::{BlockCommitter, BlockWatcher, CommitListener},
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

pub fn spawn_eth_committer_listener(
    config: &Config,
    internal_config: &InternalConfig,
    rx_fuel_block: Receiver<FuelBlock>,
    storage: InMemoryStorage,
    _registry: &Registry,
) -> Result<(
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
    HealthChecker,
)> {
    let ethereum_rpc = EthereumRPC::new(
        &config.ethereum_rpc,
        config.state_contract_address,
        &config.ethereum_wallet_key,
    );
    let eth_health_check = ethereum_rpc.connection_health_checker();

    let block_committer = BlockCommitter::new(rx_fuel_block, ethereum_rpc.clone(), storage.clone());
    let committer_handler = tokio::spawn(async move {
        block_committer.run().await.unwrap();
    });

    let commit_listener = CommitListener::new(ethereum_rpc, storage.clone());
    let listener_handle = schedule_polling(internal_config.eth_polling_interval, commit_listener);

    Ok((committer_handler, listener_handle, eth_health_check))
}

fn schedule_polling(
    polling_interval: Duration,
    runner: impl Runner + 'static,
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
