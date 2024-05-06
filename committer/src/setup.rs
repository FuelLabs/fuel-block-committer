use std::time::Duration;

use eth_rpc::WsAdapter;
use fuel_rpc::client::Client;
use metrics::{HealthChecker, RegistersMetrics};
use ports::{storage::Storage, types::FuelBlock};
use prometheus::Registry;
use services::{BlockCommitter, BlockWatcher, CommitListener, Runner, WalletBalanceTracker};
use storage::Postgres;
use tokio::{sync::mpsc::Receiver, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::{
    config::{Config, InternalConfig},
    errors::Result,
};

pub fn spawn_block_watcher(
    config: &Config,
    internal_config: &InternalConfig,
    storage: Postgres,
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
    internal_config: &InternalConfig,
    registry: &Registry,
    ethereum_rpc: WsAdapter,
    cancel_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let wallet_balance_tracker = WalletBalanceTracker::new(ethereum_rpc);

    wallet_balance_tracker.register_metrics(registry);

    schedule_polling(
        internal_config.balance_update_interval,
        wallet_balance_tracker,
        "Wallet Balance Tracker",
        cancel_token,
    )
}

pub fn spawn_eth_committer_and_listener(
    internal_config: &InternalConfig,
    rx_fuel_block: Receiver<FuelBlock>,
    ethereum_rpc: WsAdapter,
    storage: Postgres,
    registry: &Registry,
    cancel_token: CancellationToken,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
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

    (committer_handler, listener_handle)
}

fn create_block_committer(
    rx_fuel_block: Receiver<FuelBlock>,
    ethereum_rpc: WsAdapter,
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
) -> Result<(WsAdapter, HealthChecker)> {
    let ws_adapter = WsAdapter::connect(
        &config.eth.rpc,
        config.eth.chain_id,
        config.eth.state_contract_address,
        &config.eth.wallet_key,
        config.eth.commit_interval,
        internal_config.eth_errors_before_unhealthy,
    )
    .await?;

    ws_adapter.register_metrics(registry);

    let health_check = ws_adapter.connection_health_checker();

    Ok((ws_adapter, health_check))
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
) -> (Client, HealthChecker) {
    let fuel_adapter = Client::new(
        &config.fuel.graphql_endpoint,
        internal_config.fuel_errors_before_unhealthy,
    );
    fuel_adapter.register_metrics(registry);

    let fuel_connection_health = fuel_adapter.connection_health_checker();

    (fuel_adapter, fuel_connection_health)
}

fn create_block_watcher(
    config: &Config,
    registry: &Registry,
    fuel_adapter: Client,
    storage: Postgres,
) -> (BlockWatcher<Client, Postgres>, Receiver<FuelBlock>) {
    let (tx_fuel_block, rx_fuel_block) = tokio::sync::mpsc::channel(100);
    let block_watcher = BlockWatcher::new(
        config.eth.commit_interval,
        tx_fuel_block,
        fuel_adapter,
        storage,
    );
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

pub async fn setup_storage(config: &Config) -> Result<Postgres> {
    let postgres = Postgres::connect(&config.app.db).await?;
    postgres.migrate().await?;

    Ok(postgres)
}

pub async fn shut_down(
    cancel_token: CancellationToken,
    block_watcher_handle: JoinHandle<()>,
    wallet_balance_tracker_handle: JoinHandle<()>,
    committer_handle: JoinHandle<()>,
    listener_handle: JoinHandle<()>,
    storage: Postgres,
) -> Result<()> {
    cancel_token.cancel();

    for handle in [
        block_watcher_handle,
        wallet_balance_tracker_handle,
        committer_handle,
        listener_handle,
    ] {
        handle.await?;
    }

    storage.close().await;
    Ok(())
}
