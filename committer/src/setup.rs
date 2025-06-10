use std::{num::NonZeroU32, time::Duration};

use clock::SystemClock;
use eigenda::{EigenDAClient, Throughput};
use eth::{AcceptablePriorityFeePercentages, BlobEncoder};
use fuel_block_committer_encoding::bundle;
use metrics::{
    HealthChecker, RegistersMetrics,
    prometheus::{IntGauge, Registry},
};
use services::{
    BlockBundler, BlockBundlerConfig, Runner,
    block_committer::{port::l1::Contract, service::BlockCommitter},
    fees::cache::CachingApi,
    state_committer::port::Storage,
    state_listener::{eigen_service::StateListener as EigenStateListener, service::StateListener},
    state_pruner::service::StatePruner,
    wallet_balance_tracker::service::WalletBalanceTracker,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::{
    Database, EigenDA, FuelApi, L1,
    config::{self, DALayer, EigenDaConfig},
    errors::{Error, Result},
};

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

pub fn block_committer(
    l1: L1,
    storage: Database,
    fuel: FuelApi,
    config: &config::Config,
    cancel_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let commit_interval = l1.commit_interval();
    let block_committer = BlockCommitter::new(
        l1,
        storage,
        fuel,
        SystemClock,
        commit_interval,
        config.app.num_blocks_to_finalize_tx,
    );

    schedule_polling(
        config.app.block_check_interval,
        block_committer,
        "Block Committer",
        cancel_token,
    )
}

pub fn ethereum_da_services(
    fuel: FuelApi,
    ethereum_rpc: L1,
    storage: Database,
    cancel_token: CancellationToken,
    config: &config::Config,
    internal_config: &config::Internal,
    registry: &Registry,
) -> Result<Vec<tokio::task::JoinHandle<()>>> {
    let block_bundler = block_bundler(
        fuel.clone(),
        storage.clone(),
        cancel_token.clone(),
        config,
        registry,
    );

    let fee_api = CachingApi::new(
        ethereum_rpc.clone(),
        internal_config.l1_blocks_cached_for_fee_metrics_tracker,
    );

    let fee_metrics_updater_handle =
        ethereum_da_fee_metrics_tracker(fee_api.clone(), cancel_token.clone(), config, registry)?;

    let committer = state_committer(
        fuel.clone(),
        ethereum_rpc.clone(),
        storage.clone(),
        cancel_token.clone(),
        config,
        registry,
        fee_api,
    )?;

    let listener = state_listener(
        ethereum_rpc,
        storage.clone(),
        cancel_token.clone(),
        registry,
        config,
        last_finalization_metric(),
    );

    let state_importer_handle = block_importer(
        fuel,
        storage.clone(),
        cancel_token.clone(),
        config,
        internal_config,
        registry,
    );

    let state_pruner_handle = state_pruner(storage.clone(), cancel_token.clone(), registry, config);

    let handles = vec![
        committer,
        state_importer_handle,
        block_bundler,
        listener,
        fee_metrics_updater_handle,
        state_pruner_handle,
    ];

    Ok(handles)
}

pub async fn eigen_da_services(
    fuel: FuelApi,
    storage: Database,
    cancel_token: CancellationToken,
    config: &config::Config,
    internal_config: &config::Internal,
    registry: &Registry,
) -> Result<Vec<tokio::task::JoinHandle<()>>> {
    let block_bundler = eigen_block_bundler(
        fuel.clone(),
        storage.clone(),
        cancel_token.clone(),
        config,
        registry,
    );

    let (state_committer_handle, state_listener_handle, fee_metrics_handle) =
        match config.da_layer.clone() {
            Some(DALayer::EigenDA(eigen_config)) => {
                let eigen_da = eigen_adapter(&eigen_config, internal_config).await?;
                let committer = eigen_state_committer(
                    fuel.clone(),
                    eigen_da.clone(),
                    storage.clone(),
                    cancel_token.clone(),
                    config,
                    registry,
                )?;

                let listener = eigen_state_listener(
                    eigen_da.clone(),
                    storage.clone(),
                    cancel_token.clone(),
                    config,
                    registry,
                    last_finalization_metric(),
                )?;

                let fee_metrics_handle =
                    eigen_fee_metrics_tracker(eigen_da, cancel_token.clone(), config, registry)?;

                (committer, listener, fee_metrics_handle)
            }
            _ => {
                return Err(Error::Other("Invalid da layer config".to_string()));
            }
        };

    let state_importer_handle = block_importer(
        fuel,
        storage.clone(),
        cancel_token.clone(),
        config,
        internal_config,
        registry,
    );

    let state_pruner_handle = state_pruner(storage.clone(), cancel_token.clone(), registry, config);

    let handles = vec![
        state_committer_handle,
        state_importer_handle,
        block_bundler,
        state_listener_handle,
        state_pruner_handle,
        fee_metrics_handle,
    ];

    Ok(handles)
}

pub fn eigen_block_bundler(
    fuel: FuelApi,
    storage: Database,
    cancel_token: CancellationToken,
    config: &config::Config,
    registry: &Registry,
) -> tokio::task::JoinHandle<()> {
    let bundler_factory = services::EigenBundlerFactory::new(
        bundle::Encoder::new(config.app.bundle.compression_level),
        match &config.da_layer {
            // Defaults to 3.5MB, as 4+MB errors out on the server side when checking for inclusion.
            Some(DALayer::EigenDA(eigen_da)) => eigen_da
                .fragment_size
                .unwrap_or(NonZeroU32::new(3_500_000).unwrap()),
            other => panic!("Wrong DA layer configuration for Eigen bundler: {other:?}"),
        },
        config.app.bundle.max_fragments_per_bundle,
    );

    let block_bundler = BlockBundler::new(
        fuel,
        storage,
        SystemClock,
        bundler_factory,
        BlockBundlerConfig {
            optimization_time_limit: Duration::from_secs(0),
            accumulation_time_limit: config.app.bundle.accumulation_timeout,
            blocks_to_accumulate: config.app.bundle.blocks_to_accumulate,
            lookback_window: config.app.bundle.block_height_lookback,
            bytes_to_accumulate: config.app.bundle.bytes_to_accumulate,
            max_fragments_per_bundle: config.app.bundle.max_fragments_per_bundle,
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

pub fn block_bundler(
    fuel: FuelApi,
    storage: Database,
    cancel_token: CancellationToken,
    config: &config::Config,
    registry: &Registry,
) -> tokio::task::JoinHandle<()> {
    let bundler_factory = services::BundlerFactory::new(
        BlobEncoder,
        bundle::Encoder::new(config.app.bundle.compression_level),
        config.app.bundle.optimization_step,
        config.app.bundle.max_fragments_per_bundle,
    );

    let block_bundler = BlockBundler::new(
        fuel,
        storage,
        SystemClock,
        bundler_factory,
        BlockBundlerConfig {
            optimization_time_limit: config.app.bundle.optimization_timeout,
            accumulation_time_limit: config.app.bundle.accumulation_timeout,
            bytes_to_accumulate: config.app.bundle.bytes_to_accumulate,
            max_fragments_per_bundle: config.app.bundle.max_fragments_per_bundle,
            lookback_window: config.app.bundle.block_height_lookback,
            blocks_to_accumulate: config.app.bundle.blocks_to_accumulate,
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
    registry: &Registry,
    fee_api: CachingApi<L1>,
) -> Result<tokio::task::JoinHandle<()>> {
    let state_committer = services::StateCommitter::new(
        l1,
        fuel,
        storage,
        services::StateCommitterConfig {
            lookback_window: config.app.bundle.block_height_lookback,
            fragment_accumulation_timeout: config.app.bundle.fragment_accumulation_timeout,
            fragments_to_accumulate: config.app.bundle.fragments_to_accumulate,
            gas_bump_timeout: config.app.gas_bump_timeout,
            fee_algo: config.fee_algo_config(),
        },
        SystemClock,
        fee_api,
    );

    state_committer.register_metrics(registry);

    Ok(schedule_polling(
        config.app.tx_finalization_check_interval,
        state_committer,
        "State Committer",
        cancel_token,
    ))
}

pub fn eigen_state_committer(
    fuel: FuelApi,
    eigen_da: EigenDA,
    storage: Database,
    cancel_token: CancellationToken,
    config: &config::Config,
    registry: &Registry,
) -> Result<tokio::task::JoinHandle<()>> {
    let state_committer = services::EigenStateCommitter::new(
        eigen_da,
        fuel,
        storage,
        services::EigenStatecommitterConfig {
            api_throughput: 16, // TODO
            lookback_window: config.app.bundle.block_height_lookback,
        },
        SystemClock,
    );

    state_committer.register_metrics(registry);

    // TODO: have a separate configurable polling interval
    Ok(schedule_polling(
        Duration::from_secs(1),
        state_committer,
        "State Committer",
        cancel_token,
    ))
}

pub fn eigen_state_listener(
    eigen_da: EigenDA,
    storage: Database,
    cancel_token: CancellationToken,
    config: &config::Config,
    registry: &Registry,
    last_finalization: IntGauge,
) -> Result<tokio::task::JoinHandle<()>> {
    let state_committer = EigenStateListener::new(eigen_da, storage, last_finalization);

    state_committer.register_metrics(registry);

    Ok(schedule_polling(
        config.app.tx_finalization_check_interval,
        state_committer,
        "State Listener",
        cancel_token,
    ))
}

pub fn block_importer(
    fuel: FuelApi,
    storage: Database,
    cancel_token: CancellationToken,
    config: &config::Config,
    internal_config: &config::Internal,
    registry: &Registry,
) -> tokio::task::JoinHandle<()> {
    let block_importer = services::block_importer::service::BlockImporter::new(
        storage,
        fuel,
        config.app.bundle.block_height_lookback,
        internal_config.import_batches.max_blocks,
        internal_config.import_batches.max_cumulative_size,
    );

    // Register metrics for the block importer
    block_importer.register_metrics(registry);

    schedule_polling(
        config.app.block_check_interval,
        block_importer,
        "State Importer",
        cancel_token,
    )
}

pub fn last_finalization_metric() -> IntGauge {
    IntGauge::new(
        "seconds_since_last_finalized_fragment",
        "The number of seconds since the last finalized fragment",
    )
    .expect("seconds_since_last_finalized_fragment gauge to be correctly configured")
}

pub fn state_listener(
    l1: L1,
    storage: Database,
    cancel_token: CancellationToken,
    registry: &Registry,
    config: &config::Config,
    last_finalization: IntGauge,
) -> tokio::task::JoinHandle<()> {
    let state_listener = StateListener::new(
        l1,
        storage,
        config.app.num_blocks_to_finalize_tx,
        SystemClock,
        last_finalization,
    );

    state_listener.register_metrics(registry);

    schedule_polling(
        config.app.block_check_interval,
        state_listener,
        "State Listener",
        cancel_token,
    )
}

pub fn state_pruner(
    storage: Database,
    cancel_token: CancellationToken,
    registry: &Registry,
    config: &config::Config,
) -> tokio::task::JoinHandle<()> {
    let state_pruner = StatePruner::new(storage, SystemClock, config.app.state_pruner_retention);

    state_pruner.register_metrics(registry);

    schedule_polling(
        config.app.state_pruner_run_interval,
        state_pruner,
        "State Listener",
        cancel_token,
    )
}

pub async fn l1_adapter(
    config: &config::Config,
    internal_config: &config::Internal,
    registry: &Registry,
) -> Result<(L1, HealthChecker)> {
    let l1 = L1::connect(
        config.eth.rpc.clone(),
        config.eth.state_contract_address,
        config.eth.signers().await?,
        internal_config.eth_errors_before_unhealthy,
        eth::TxConfig {
            tx_max_fee: u128::from(config.app.tx_fees.max),
            send_tx_request_timeout: config.app.send_tx_request_timeout,
            acceptable_priority_fee_percentage: AcceptablePriorityFeePercentages::new(
                config.app.tx_fees.min_reward_perc,
                config.app.tx_fees.max_reward_perc,
            )?,
        },
    )
    .await?;

    l1.register_metrics(registry);

    let health_check = l1.connection_health_checker();

    Ok((l1, health_check))
}

pub async fn eigen_adapter(
    config: &EigenDaConfig,
    _internal_config: &config::Internal,
) -> Result<EigenDA> {
    // Configure with appropriate throughput values
    // because testing showed that the time window in which thottling is calculated allows for
    // around 500MB to be sent before throttling comes into effect
    let burst = 16_000_000.try_into().unwrap();
    let eigen_da = EigenDAClient::new(
        config.signer().await?,
        config.rpc.clone(),
        Throughput {
            bytes_per_sec: 2_000_000.try_into().unwrap(),
            max_burst: burst,
            calls_per_sec: 1.try_into().unwrap(),
        },
    )
    .await
    .map_err(|e| Error::Other(format!("Failed to initialize EigenDA client: {}", e)))?;

    Ok(eigen_da)
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
        config.fuel.num_buffered_requests,
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

pub async fn storage(
    config: &config::Config,
    registry: &Registry,
    last_finalization: &IntGauge,
) -> Result<Database> {
    let postgres = Database::connect(&config.app.db).await?;
    postgres.migrate().await?;

    postgres.register_metrics(registry);

    if let Some(last_fragment_time) = postgres.last_time_a_fragment_was_finalized().await? {
        last_finalization.set(last_fragment_time.timestamp());
    }

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

pub fn ethereum_da_fee_metrics_tracker(
    api: CachingApi<L1>,
    cancel_token: CancellationToken,
    config: &config::Config,
    registry: &Registry,
) -> Result<tokio::task::JoinHandle<()>> {
    use services::fee_metrics_tracker::ethereum_da::service::FeeMetricsTracker;

    let fee_metrics_tracker = FeeMetricsTracker::new(api);

    fee_metrics_tracker.register_metrics(registry);

    let handle = schedule_polling(
        config.app.l1_fee_check_interval,
        fee_metrics_tracker,
        "Fee Tracker",
        cancel_token,
    );

    Ok(handle)
}

pub fn eigen_fee_metrics_tracker<S: Send + Sync + 'static>(
    eigen_da_client: EigenDAClient<S>,
    cancel_token: CancellationToken,
    config: &config::Config,
    registry: &Registry,
) -> Result<tokio::task::JoinHandle<()>> {
    use services::fee_metrics_tracker::eigen_da::service::FeeMetricsTracker;

    let fee_metrics_tracker = FeeMetricsTracker::new(eigen_da_client);

    fee_metrics_tracker.register_metrics(registry);

    let polling_interval = config
        .da_layer
        .as_ref()
        .map(|layer| match layer {
            DALayer::EigenDA(eigenda_config) => eigenda_config.fee_check_interval,
        })
        .ok_or_else(|| {
            Error::Other("Invalid DA layer configuration for Eigen fee metrics tracker".to_string())
        })?;

    let handle = schedule_polling(
        polling_interval,
        fee_metrics_tracker,
        "Fee Tracker", // has the same name as the Ethereum DA fee tracker, all other eigen specific modules have the same name as the Ethereum DA modules
        cancel_token,
    );

    Ok(handle)
}
