use std::time::Duration;

use prometheus::Registry;
use tokio::{sync::mpsc::Receiver, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::{
    adapters::{
        ethereum_adapter::{EthereumWs, MonitoredEthAdapter},
        fuel_adapter::{FuelBlock, FuelClient},
        runner::Runner,
        storage::{sqlite_db::SqliteDb, Storage},
    },
    errors::Result,
    services::{BlockCommitter, BlockWatcher, CommitListener, WalletBalanceTracker},
    setup::config::{Config, InternalConfig},
    telemetry::{HealthChecker, RegistersMetrics},
};

pub fn spawn_block_watcher(
    config: &Config,
    internal_config: &InternalConfig,
    storage: impl Storage + 'static,
    registry: &Registry,
    cancel_token: CancellationToken,
) -> (
    Receiver<FuelBlock>,
    tokio::task::JoinHandle<()>,
    HealthChecker,
) {
    let (fuel_adapter, fuel_connection_health) =
        create_fuel_adapter(config, internal_config, registry);

    let (block_watcher, rx) = create_block_watcher(config, registry, fuel_adapter, storage);

    let handle = schedule_polling(
        internal_config.fuel_polling_interval,
        block_watcher,
        "Block Watcher",
        cancel_token,
    );

    (rx, handle, fuel_connection_health)
}

pub fn spawn_wallet_balance_tracker(
    config: &Config,
    internal_config: &InternalConfig,
    registry: &Registry,
    ethereum_rpc: MonitoredEthAdapter<EthereumWs>,
    cancel_token: CancellationToken,
) -> Result<tokio::task::JoinHandle<()>> {
    let wallet_balance_tracker =
        WalletBalanceTracker::new(ethereum_rpc, &config.ethereum_wallet_key);

    wallet_balance_tracker.register_metrics(registry);

    let listener_handle = schedule_polling(
        internal_config.balance_update_interval,
        wallet_balance_tracker,
        "Wallet Balance Tracker",
        cancel_token,
    );

    Ok(listener_handle)
}

pub fn spawn_eth_committer_and_listener(
    internal_config: &InternalConfig,
    rx_fuel_block: Receiver<FuelBlock>,
    ethereum_rpc: MonitoredEthAdapter<EthereumWs>,
    storage: SqliteDb,
    registry: &Registry,
    cancel_token: CancellationToken,
) -> Result<(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>)> {
    let committer_handler =
        create_block_committer(rx_fuel_block, ethereum_rpc.clone(), storage.clone());

    let commit_listener = CommitListener::new(ethereum_rpc, storage, cancel_token.clone());
    commit_listener.register_metrics(registry);

    let listener_handle = schedule_polling(
        internal_config.between_eth_event_stream_restablishing_attempts,
        commit_listener,
        "Commit Listener",
        cancel_token,
    );

    Ok((committer_handler, listener_handle))
}

fn create_block_committer(
    rx_fuel_block: Receiver<FuelBlock>,
    ethereum_rpc: MonitoredEthAdapter<EthereumWs>,
    storage: impl Storage + 'static,
) -> tokio::task::JoinHandle<()> {
    let mut block_committer = BlockCommitter::new(rx_fuel_block, ethereum_rpc, storage);
    tokio::spawn(async move {
        block_committer
            .run()
            .await
            .expect("Errors are handled inside of run");
    })
}

pub async fn create_eth_adapter(
    config: &Config,
    internal_config: &InternalConfig,
    registry: &Registry,
) -> Result<(MonitoredEthAdapter<EthereumWs>, HealthChecker)> {
    let ethereum_rpc = EthereumWs::connect(
        &config.ethereum_rpc,
        config.ethereum_chain_id,
        config.state_contract_address,
        &config.ethereum_wallet_key,
        config.commit_interval,
    )
    .await?;

    let monitored =
        MonitoredEthAdapter::new(ethereum_rpc, internal_config.eth_errors_before_unhealthy);
    monitored.register_metrics(registry);

    let health_check = monitored.connection_health_checker();

    Ok((monitored, health_check))
}

fn schedule_polling(
    polling_interval: Duration,
    mut runner: impl Runner + 'static,
    name: &'static str,
    cancel_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(e) = runner.run().await {
                error!("{name} encountered an error: {e}");
            }

            if cancel_token.is_cancelled() {
                break;
            }

            tokio::time::sleep(polling_interval).await;
        }

        info!("{name} stopped");
    })
}

fn create_fuel_adapter(
    config: &Config,
    internal_config: &InternalConfig,
    registry: &Registry,
) -> (FuelClient, HealthChecker) {
    let fuel_adapter = FuelClient::new(
        &config.fuel_graphql_endpoint,
        internal_config.fuel_errors_before_unhealthy,
    );
    fuel_adapter.register_metrics(registry);

    let fuel_connection_health = fuel_adapter.connection_health_checker();

    (fuel_adapter, fuel_connection_health)
}

fn create_block_watcher(
    config: &Config,
    registry: &Registry,
    fuel_adapter: FuelClient,
    storage: impl Storage + 'static,
) -> (BlockWatcher, Receiver<FuelBlock>) {
    let (tx_fuel_block, rx_fuel_block) = tokio::sync::mpsc::channel(100);
    let block_watcher =
        BlockWatcher::new(config.commit_interval, tx_fuel_block, fuel_adapter, storage);
    block_watcher.register_metrics(registry);

    (block_watcher, rx_fuel_block)
}

pub fn setup_logger() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_level(true)
        .with_line_number(true)
        .json()
        .init();
}

pub async fn setup_storage(config: &Config) -> Result<SqliteDb> {
    if let Some(path) = &config.db_path {
        SqliteDb::open(path).await
    } else {
        SqliteDb::temporary().await
    }
}

pub async fn shut_down(
    cancel_token: CancellationToken,
    block_watcher_handle: JoinHandle<()>,
    wallet_balance_tracker_handle: JoinHandle<()>,
    committer_handle: JoinHandle<()>,
    listener_handle: JoinHandle<()>,
) -> Result<()> {
    cancel_token.cancel();

    for handle in [
        block_watcher_handle,
        wallet_balance_tracker_handle,
        committer_handle,
        listener_handle,
    ] {
        handle.await?
    }

    Ok(())
}
