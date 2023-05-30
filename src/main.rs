<<<<<<< HEAD
use crate::errors::Result;
use adapters::{ethereum_rpc::EthereumRPC, storage::InMemoryStorage};
=======
use adapters::storage::InMemoryStorage;
>>>>>>> origin/ahmed-hal3e/dev3
use api::launch_api_server;
use prometheus::Registry;

use setup::{
    config::InternalConfig,
<<<<<<< HEAD
    helpers::{schedule_polling, setup_logger, spawn_fake_block_watcher},
};
use telemetry::RegistersMetrics;

use adapters::runner::Runner;
=======
    helpers::{setup_logger, spawn_eth_committer_listener, spawn_fake_block_watcher},
};

use crate::errors::Result;
>>>>>>> origin/ahmed-hal3e/dev3

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

    setup_logger();

    let internal_config = InternalConfig::default();
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

<<<<<<< HEAD
    // Ethereum provider
    let ethereum_rpc = EthereumRPC::new(
        &config.ethereum_rpc,
        config.state_contract_address,
        &config.ethereum_wallet_key,
    );
    ethereum_rpc.register_metrics(&metrics_registry);
    let eth_health_check = ethereum_rpc.connection_health_checker();

    // service BlockCommitter
    let block_committer = BlockCommitter::new(rx_fuel_block, ethereum_rpc.clone(), storage.clone());
    tokio::spawn(async move {
        block_committer.run().await.unwrap();
    });

    // service CommitListener
    let commit_listener = CommitListener::new(ethereum_rpc, storage.clone());
    let handle = schedule_polling(internal_config.eth_polling_interval, commit_listener);
=======
    let (_committer_handle, _listener_handle, eth_health_check) = spawn_eth_committer_listener(
        &config,
        &internal_config,
        rx_fuel_block,
        storage.clone(),
        &metrics_registry,
    )?;
>>>>>>> origin/ahmed-hal3e/dev3

    launch_api_server(
        &config,
        metrics_registry,
        storage,
        fuel_health_check,
        eth_health_check,
    )
    .await?;

    Ok(())
}
