use std::time::Duration;

use fuels::types::block::Block as FuelBlock;
use prometheus::Registry;
use tokio::sync::mpsc::Receiver;
use tracing::error;

use crate::{
    adapters::{
        block_fetcher::FuelBlockFetcher,
        ethereum_adapter::EthereumRPC,
        runner::Runner,
        storage::{sqlite_db::SqliteDb, Storage},
    },
    errors::Result,
    services::{BlockCommitter, BlockWatcher, CommitListener},
    setup::config::{Config, InternalConfig},
    telemetry::{HealthChecker, RegistersMetrics},
};

pub fn spawn_block_watcher(
    config: &Config,
    internal_config: &InternalConfig,
    storage: impl Storage + 'static,
    registry: &Registry,
) -> (
    Receiver<FuelBlock>,
    tokio::task::JoinHandle<()>,
    HealthChecker,
) {
    let (block_fetcher, fuel_connection_health) =
        create_block_fetcher(config, internal_config, registry);

    let (block_watcher, rx) = create_block_watcher(config, registry, block_fetcher, storage);

    let handle = schedule_polling(
        internal_config.fuel_polling_interval,
        block_watcher,
        "Block Watcher",
    );

    (rx, handle, fuel_connection_health)
}

pub fn spawn_eth_committer_and_listener(
    internal_config: &InternalConfig,
    rx_fuel_block: Receiver<FuelBlock>,
    ethereum_rpc: EthereumRPC,
    storage: SqliteDb,
) -> Result<(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>)> {
    let committer_handler =
        create_block_committer(rx_fuel_block, ethereum_rpc.clone(), storage.clone());

    let listener_handle = schedule_polling(
        internal_config.eth_polling_interval,
        CommitListener::new(ethereum_rpc, storage),
        "Commit Listener",
    );

    Ok((committer_handler, listener_handle))
}

fn create_block_committer(
    rx_fuel_block: Receiver<FuelBlock>,
    ethereum_rpc: EthereumRPC,
    storage: impl Storage + 'static,
) -> tokio::task::JoinHandle<()> {
    let block_committer = BlockCommitter::new(rx_fuel_block, ethereum_rpc, storage);
    tokio::spawn(async move {
        block_committer
            .run()
            .await
            .expect("Errors are handled inside of run");
    })
}

pub async fn create_eth_rpc(
    config: &Config,
    internal_config: &InternalConfig,
    registry: &Registry,
) -> Result<(EthereumRPC, HealthChecker)> {
    let ethereum_rpc = EthereumRPC::connect(
        &config.ethereum_rpc,
        config.ethereum_chain_id,
        config.state_contract_address,
        &config.ethereum_wallet_key,
        internal_config.eth_errors_before_unhealthy,
    )
    .await?;
    ethereum_rpc.register_metrics(registry);

    let eth_health_check = ethereum_rpc.connection_health_checker();
    Ok((ethereum_rpc, eth_health_check))
}

fn schedule_polling(
    polling_interval: Duration,
    runner: impl Runner + 'static,
    name: &'static str,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(e) = runner.run().await {
                error!("{name} encountered an error: {e}");
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
    storage: impl Storage + 'static,
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

pub async fn setup_storage(config: &Config) -> Result<SqliteDb> {
    if let Some(path) = &config.db_path {
        SqliteDb::open(path).await
    } else {
        SqliteDb::temporary().await
    }
}
