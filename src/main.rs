mod adapters;
mod api;
mod cli;
mod errors;
mod services;
mod setup;
mod telemetry;

use api::launch_api_server;
use errors::Result;
use prometheus::Registry;
use setup::{
    config::InternalConfig,
    helpers::{
        create_eth_adapter, setup_logger, setup_storage, shut_down, spawn_block_watcher,
        spawn_eth_committer_and_listener, spawn_wallet_balance_tracker,
    },
};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    setup_logger();

    let config = cli::parse()?;
    let internal_config = InternalConfig::default();
    let cancel_token = CancellationToken::new();

    let storage = setup_storage(&config).await?;

    let metrics_registry = Registry::default();

    let (rx_fuel_block, block_watcher_handle, fuel_health_check) = spawn_block_watcher(
        &config,
        &internal_config,
        storage.clone(),
        &metrics_registry,
        cancel_token.clone(),
    );

    let (ethereum_rpc, eth_health_check) =
        create_eth_adapter(&config, &internal_config, &metrics_registry).await?;

    let wallet_balance_tracker_handle = spawn_wallet_balance_tracker(
        &config,
        &internal_config,
        &metrics_registry,
        ethereum_rpc.clone(),
        cancel_token.clone(),
    )?;

    let (committer_handle, listener_handle) = spawn_eth_committer_and_listener(
        &internal_config,
        rx_fuel_block,
        ethereum_rpc,
        storage.clone(),
        &metrics_registry,
        cancel_token.clone(),
    )?;

    launch_api_server(
        &config,
        metrics_registry,
        storage,
        fuel_health_check,
        eth_health_check,
    )
    .await?;

    shut_down(
        cancel_token,
        block_watcher_handle,
        wallet_balance_tracker_handle,
        committer_handle,
        listener_handle,
    )
    .await
}
