use std::num::NonZeroU32;
use std::time::Duration;

use metrics::{prometheus::Registry, HealthChecker, RegistersMetrics};
use ports::{storage::Storage, types::ValidatedFuelBlock};
use services::{BlockCommitter, BlockWatcher, CommitListener, Runner, WalletBalanceTracker};
use tokio::{sync::mpsc::Receiver, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use validator::BlockValidator;

use crate::{
    config::{Config, InternalConfig},
    errors::Result,
    Database, FuelApi, Validator, L1,
};

pub fn spawn_block_watcher(
    commit_interval: NonZeroU32,
    config: &Config,
    internal_config: &InternalConfig,
    storage: Database,
    registry: &Registry,
    cancel_token: CancellationToken,
) -> (
    Receiver<ValidatedFuelBlock>,
    tokio::task::JoinHandle<()>,
    HealthChecker,
) {
    let (fuel_adapter, fuel_connection_health) =
        create_fuel_adapter(config, internal_config, registry);

    let (block_watcher, rx) =
        create_block_watcher(commit_interval, config, registry, fuel_adapter, storage);

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
    l1: L1,
    cancel_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let wallet_balance_tracker = WalletBalanceTracker::new(l1);

    wallet_balance_tracker.register_metrics(registry);

    schedule_polling(
        internal_config.balance_update_interval,
        wallet_balance_tracker,
        "Wallet Balance Tracker",
        cancel_token,
    )
}

pub fn spawn_l1_committer_and_listener(
    internal_config: &InternalConfig,
    rx_fuel_block: Receiver<ValidatedFuelBlock>,
    l1: L1,
    storage: Database,
    registry: &Registry,
    cancel_token: CancellationToken,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let committer_handler = create_block_committer(rx_fuel_block, l1.clone(), storage.clone());

    let commit_listener = CommitListener::new(l1, storage, cancel_token.clone());
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
    rx_fuel_block: Receiver<ValidatedFuelBlock>,
    l1: L1,
    storage: impl Storage + 'static,
) -> tokio::task::JoinHandle<()> {
    let mut block_committer = BlockCommitter::new(rx_fuel_block, l1, storage);
    tokio::spawn(async move {
        block_committer
            .run()
            .await
            .expect("Errors are handled inside of run");
    })
}

pub async fn create_l1_adapter(
    config: &Config,
    internal_config: &InternalConfig,
    registry: &Registry,
) -> Result<(L1, HealthChecker)> {
    let l1 = L1::connect(
        &config.eth.rpc,
        config.eth.chain_id,
        config.eth.state_contract_address,
        &config.eth.wallet_key,
        internal_config.eth_errors_before_unhealthy,
    )
    .await?;

    l1.register_metrics(registry);

    let health_check = l1.connection_health_checker();

    Ok((l1, health_check))
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
) -> (FuelApi, HealthChecker) {
    let fuel_adapter = FuelApi::new(
        &config.fuel.graphql_endpoint,
        internal_config.fuel_errors_before_unhealthy,
    );
    fuel_adapter.register_metrics(registry);

    let fuel_connection_health = fuel_adapter.connection_health_checker();

    (fuel_adapter, fuel_connection_health)
}

fn create_block_watcher(
    commit_interval: NonZeroU32,
    config: &Config,
    registry: &Registry,
    fuel_adapter: FuelApi,
    storage: Database,
) -> (
    BlockWatcher<FuelApi, Database, Validator>,
    Receiver<ValidatedFuelBlock>,
) {
    let (tx_fuel_block, rx_fuel_block) = tokio::sync::mpsc::channel(100);
    let block_watcher = BlockWatcher::new(
        commit_interval,
        tx_fuel_block,
        fuel_adapter,
        storage,
        BlockValidator::new(config.fuel.block_producer_public_key),
    );
    block_watcher.register_metrics(registry);

    (block_watcher, rx_fuel_block)
}

pub fn setup_logger() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_level(true)
        .with_line_number(true)
        .json()
        .init();
}

pub async fn setup_storage(config: &Config) -> Result<Database> {
    let postgres = Database::connect(&config.app.db).await?;
    postgres.migrate().await?;

    Ok(postgres)
}

pub async fn shut_down(
    cancel_token: CancellationToken,
    block_watcher_handle: JoinHandle<()>,
    wallet_balance_tracker_handle: JoinHandle<()>,
    committer_handle: JoinHandle<()>,
    listener_handle: JoinHandle<()>,
    storage: Database,
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
