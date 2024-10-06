use std::{num::NonZeroU32, time::Duration};

use clock::SystemClock;
use eth::{AwsConfig, Eip4844BlobEncoder};
use metrics::{prometheus::Registry, HealthChecker, RegistersMetrics};
use services::{
    BlockBundler, BlockBundlerConfig, BlockCommitter, BlockValidator, CommitListener, Runner,
    WalletBalanceTracker,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::{config, errors::Result, AwsClient, Database, FuelApi, L1};

pub fn wallet_balance_tracker(
    internal_config: &config::Internal,
    registry: &Registry,
    l1: L1,
    cancel_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let contract_caller_address = l1.contract_caller_address();
    let blob_posting_address = l1.blob_poster_address();
    let mut wallet_balance_tracker = WalletBalanceTracker::new(l1);

    wallet_balance_tracker.track_address("contract_caller", contract_caller_address);
    if let Some(address) = blob_posting_address {
        wallet_balance_tracker.track_address("blob_poster", address);
    }

    // to be called only after all `track_address` calls
    wallet_balance_tracker.register_metrics(registry);

    schedule_polling(
        internal_config.balance_update_interval,
        wallet_balance_tracker,
        "Wallet Balance Tracker",
        cancel_token,
    )
}

pub fn l1_event_listener(
    internal_config: &config::Internal,
    l1: L1,
    storage: Database,
    registry: &Registry,
    cancel_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let commit_listener_service = CommitListener::new(l1, storage, cancel_token.clone());
    commit_listener_service.register_metrics(registry);

    schedule_polling(
        internal_config.between_eth_event_stream_restablishing_attempts,
        commit_listener_service,
        "Commit Listener",
        cancel_token,
    )
}

pub fn block_committer(
    commit_interval: NonZeroU32,
    l1: L1,
    storage: Database,
    fuel: FuelApi,
    config: &config::Config,
    cancel_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let validator = BlockValidator::new(*config.fuel.block_producer_address);

    let block_committer = BlockCommitter::new(l1, storage, fuel, validator, commit_interval);

    schedule_polling(
        config.app.block_check_interval,
        block_committer,
        "Block Committer",
        cancel_token,
    )
}

pub fn block_bundler(
    fuel: FuelApi,
    storage: Database,
    cancel_token: CancellationToken,
    config: &config::Config,
    registry: &Registry,
) -> tokio::task::JoinHandle<()> {
    let bundler_factory = services::BundlerFactory::new(
        Eip4844BlobEncoder,
        config.app.bundle.compression_level,
        config.app.bundle.optimization_step,
    );

    let block_bundler = BlockBundler::new(
        fuel,
        storage,
        SystemClock,
        bundler_factory,
        BlockBundlerConfig {
            optimization_time_limit: config.app.bundle.optimization_timeout,
            block_accumulation_time_limit: config.app.bundle.accumulation_timeout,
            num_blocks_to_accumulate: config.app.bundle.blocks_to_accumulate,
            lookback_window: config.app.bundle.block_height_lookback,
            max_bundles_per_optimization_run: num_cpus::get()
                .try_into()
                .expect("num cpus not zero"),
        },
    );

    block_bundler.register_metrics(registry);

    schedule_polling(
        config.app.bundle.new_bundle_check_interval,
        block_bundler,
        "Block Bundler",
        cancel_token,
    )
}

pub fn state_committer(
    fuel: FuelApi,
    l1: L1,
    storage: Database,
    cancel_token: CancellationToken,
    config: &config::Config,
) -> tokio::task::JoinHandle<()> {
    let state_committer = services::StateCommitter::new(
        l1,
        fuel,
        storage,
        services::StateCommitterConfig {
            lookback_window: config.app.bundle.block_height_lookback,
            fragment_accumulation_timeout: config.app.bundle.fragment_accumulation_timeout,
            fragments_to_accumulate: config.app.bundle.fragments_to_accumulate,
        },
        SystemClock,
    );

    schedule_polling(
        config.app.tx_finalization_check_interval,
        state_committer,
        "State Committer",
        cancel_token,
    )
}

pub fn block_importer(
    fuel: FuelApi,
    storage: Database,
    cancel_token: CancellationToken,
    config: &config::Config,
) -> tokio::task::JoinHandle<()> {
    let validator = BlockValidator::new(*config.fuel.block_producer_address);
    let block_importer = services::BlockImporter::new(
        storage,
        fuel,
        validator,
        config.app.bundle.block_height_lookback,
    );

    schedule_polling(
        config.app.block_check_interval,
        block_importer,
        "State Importer",
        cancel_token,
    )
}

pub fn state_listener(
    l1: L1,
    storage: Database,
    cancel_token: CancellationToken,
    registry: &Registry,
    config: &config::Config,
) -> tokio::task::JoinHandle<()> {
    let state_listener = services::StateListener::new(
        l1,
        storage,
        config.app.num_blocks_to_finalize_tx,
        SystemClock,
    );

    state_listener.register_metrics(registry);

    schedule_polling(
        config.app.block_check_interval,
        state_listener,
        "State Listener",
        cancel_token,
    )
}

pub async fn l1_adapter(
    config: &config::Config,
    internal_config: &config::Internal,
    registry: &Registry,
) -> Result<(L1, HealthChecker)> {
    let aws_config = AwsConfig::from_env().await;

    let aws_client = AwsClient::new(aws_config);

    let l1 = L1::connect(
        config.eth.rpc.clone(),
        config.eth.state_contract_address,
        config.eth.main_key_arn.clone(),
        config.eth.blob_pool_key_arn.clone(),
        internal_config.eth_errors_before_unhealthy,
        aws_client,
        config.eth.first_tx_gas_estimation_multiplier,
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

pub fn fuel_adapter(
    config: &config::Config,
    internal_config: &config::Internal,
    registry: &Registry,
) -> (FuelApi, HealthChecker) {
    let fuel_adapter = FuelApi::new(
        &config.fuel.graphql_endpoint,
        internal_config.fuel_errors_before_unhealthy,
        config.fuel.max_full_blocks_per_request,
    );
    fuel_adapter.register_metrics(registry);

    let fuel_connection_health = fuel_adapter.connection_health_checker();

    (fuel_adapter, fuel_connection_health)
}

pub fn logger() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_level(true)
        .with_line_number(true)
        .json()
        .init();
}

pub async fn storage(config: &config::Config, registry: &Registry) -> Result<Database> {
    let postgres = Database::connect(&config.app.db).await?;
    postgres.migrate().await?;

    postgres.register_metrics(registry);

    Ok(postgres)
}

pub async fn shut_down(
    cancel_token: CancellationToken,
    handles: Vec<JoinHandle<()>>,
    storage: Database,
) -> Result<()> {
    cancel_token.cancel();

    for handle in handles {
        handle.await?;
    }

    storage.close().await;
    Ok(())
}
