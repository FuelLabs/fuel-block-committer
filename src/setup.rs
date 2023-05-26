use std::time::Duration;

use actix_web::dev::Url;
use fuels::{
    accounts::fuel_crypto::fuel_types::Bytes20, tx::Bytes32, types::block::Block as FuelBlock,
};
use prometheus::Registry;
use tokio::sync::mpsc::Receiver;
use tracing::error;

use crate::{
    adapters::{block_fetcher::FuelBlockFetcher, storage::InMemoryStorage},
    errors::Result,
    health_check::HealthChecker,
    metrics::RegistersMetrics,
    services::BlockWatcher,
};

#[derive(Default, Debug, Clone)]
pub struct Config {
    pub ethereum_wallet_key: Bytes32,
    pub ethereum_rpc: Url,
    pub fuel_graphql_endpoint: Url,
    pub state_contract_address: Bytes20,
    pub commit_epoch: u32,
}

#[derive(Debug, Clone)]
pub struct ExtraConfig {
    fuel_polling_interval: Duration,
}

impl Default for ExtraConfig {
    fn default() -> Self {
        Self {
            fuel_polling_interval: Duration::from_secs(3),
        }
    }
}

pub fn spawn_block_watcher(
    config: &Config,
    extra_config: &ExtraConfig,
    storage: InMemoryStorage,
    registry: &Registry,
) -> Result<(
    Receiver<FuelBlock>,
    tokio::task::JoinHandle<()>,
    HealthChecker,
)> {
    let (block_fetcher, fuel_connection_health) = create_block_fetcher(config, registry);

    let (block_watcher, rx) = create_block_watcher(config, registry, block_fetcher, storage);

    let handle = schedule_polling(extra_config, block_watcher);

    Ok((rx, handle, fuel_connection_health))
}

fn schedule_polling(
    config: &ExtraConfig,
    block_watcher: BlockWatcher,
) -> tokio::task::JoinHandle<()> {
    let polling_interval = config.fuel_polling_interval;
    tokio::spawn(async move {
        loop {
            if let Err(e) = block_watcher.run().await {
                error!("Block watcher encountered an error: {e}");
            }
            tokio::time::sleep(polling_interval).await;
        }
    })
}

fn create_block_fetcher(config: &Config, registry: &Registry) -> (FuelBlockFetcher, HealthChecker) {
    let block_fetcher = FuelBlockFetcher::new(&config.fuel_graphql_endpoint);
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
    let block_watcher = BlockWatcher::new(
        config.commit_epoch,
        tx_fuel_block,
        block_fetcher,
        storage.clone(),
    );
    block_watcher.register_metrics(registry);

    (block_watcher, rx_fuel_block)
}
