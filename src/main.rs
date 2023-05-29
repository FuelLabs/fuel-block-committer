use adapters::storage::InMemoryStorage;
use api::launch_api_server;
use prometheus::Registry;
use services::{BlockCommitter, CommitListener};
use setup::{
    config::InternalConfig,
    helpers::{setup_logger, spawn_block_watcher, spawn_fake_block_watcher},
};
use tracing::log::warn;

use crate::errors::Result;
use std::str::FromStr;

use adapters::tx_submitter::EthTxSubmitter;
use ethers::{
    signers::{LocalWallet, Signer},
    types::Chain,
};

mod adapters;
mod api;
mod cli;
mod common;
mod errors;
mod services;
mod setup;
mod telemetry;

#[tokio::main]
async fn main() -> Result<()> {
    let config = cli::parse()?;

    let internal_config = InternalConfig::default();

    setup_logger();
    warn!("{config:?}");

    let storage = InMemoryStorage::new();

    let metrics_registry = Registry::default();

    // let (rx_fuel_block, _block_watcher_handle, fuel_health_check) = spawn_block_watcher(
    //     &config,
    //     &internal_config,
    //     storage.clone(),
    //     &metrics_registry,
    // )?;

    let (rx_fuel_block, _block_watcher_handle, fuel_health_check) = spawn_fake_block_watcher(
        &config,
        &internal_config,
        storage.clone(),
        &metrics_registry,
    )?;

    let wallet = LocalWallet::from_str(&config.ethereum_wallet_key)
        .unwrap()
        .with_chain_id(Chain::AnvilHardhat);

    let tx_submitter = EthTxSubmitter::new(
        config.ethereum_rpc.clone(),
        config.state_contract_address,
        wallet.clone(),
    );
    let mut block_committer = BlockCommitter::new(rx_fuel_block, tx_submitter, storage.clone());
    // service BlockCommitter
    tokio::spawn(async move {
        block_committer.run().await.unwrap();
    });

    // service CommitListener
    let commit_listener = CommitListener::new(config.ethereum_rpc.clone(), storage.clone());
    // // run the service
    tokio::spawn(async move {
        commit_listener.run().await.unwrap();
    });

    launch_api_server(&config, metrics_registry, storage, fuel_health_check).await?;

    Ok(())
}
