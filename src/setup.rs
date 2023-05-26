use std::time::Duration;

use actix_web::dev::Url;
use fuels::{
    accounts::fuel_crypto::fuel_types::Bytes20, tx::Bytes32, types::block::Block as FuelBlock,
};
use prometheus::Registry;
use tokio::sync::mpsc::Receiver;

use crate::{
    adapters::{block_fetcher::FuelBlockFetcher, storage::InMemoryStorage},
    errors::Result,
    health_check::HealthCheck,
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

pub async fn spawn_block_watcher(
    config: &Config,
    extra_config: &ExtraConfig,
    storage: InMemoryStorage,
    registry: &Registry,
) -> Result<(
    Receiver<FuelBlock>,
    tokio::task::JoinHandle<()>,
    Box<dyn HealthCheck + Send + Sync>,
)> {
    let block_fetcher = FuelBlockFetcher::new(&config.fuel_graphql_endpoint);
    block_fetcher.register_metrics(registry);

    let fuel_connection_health = block_fetcher.connection_health_checker();

    let (tx_fuel_block, rx_fuel_block) = tokio::sync::mpsc::channel(100);

    let block_watcher = BlockWatcher::new(
        config.commit_epoch,
        tx_fuel_block,
        block_fetcher,
        storage.clone(),
    );
    block_watcher.register_metrics(registry);

    let polling_interval = extra_config.fuel_polling_interval;
    let handle = tokio::spawn(async move {
        loop {
            if let Err(e) = block_watcher.run().await {
                eprint!("An error with the block watcher: {e}");
            }
            tokio::time::sleep(polling_interval).await;
        }
    });

    Ok((rx_fuel_block, handle, fuel_connection_health))
}
