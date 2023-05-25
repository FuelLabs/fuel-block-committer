use std::time::Duration;

use actix_web::dev::Url;
use fuels::{
    accounts::fuel_crypto::fuel_types::Bytes20, tx::Bytes32, types::block::Block as FuelBlock,
};
use tokio::sync::mpsc::Sender;

use crate::{
    adapters::{block_fetcher::FuelBlockFetcher, storage::InMemoryStorage},
    errors::Result,
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
    tx_fuel_block: Sender<FuelBlock>,
) -> Result<tokio::task::JoinHandle<()>> {
    let block_fetcher = FuelBlockFetcher::connect(&config.fuel_graphql_endpoint).await?;

    let block_watcher = BlockWatcher::new(
        config.commit_epoch,
        tx_fuel_block,
        block_fetcher,
        storage.clone(),
    );

    let polling_interval = extra_config.fuel_polling_interval;
    Ok(tokio::spawn(async move {
        loop {
            block_watcher.run().await.unwrap();
            tokio::time::sleep(polling_interval).await;
        }
    }))
}