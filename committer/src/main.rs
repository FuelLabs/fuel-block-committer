#![deny(unused_crate_dependencies)]
mod api;
mod config;
mod errors;
mod setup;

use api::launch_api_server;
use errors::Result;
use metrics::prometheus::Registry;
use tokio_util::sync::CancellationToken;

use crate::setup::shut_down;
use ports::l1::Contract;

pub type L1 = eth::WebsocketClient;
pub type Database = storage::Postgres;
pub type FuelApi = fuel::HttpClient;
pub type Validator = validator::BlockValidator;

#[tokio::main]
async fn main() -> Result<()> {
    setup::logger();

    let config = config::parse()?;
    config.validate()?;

    let storage = setup::storage(&config).await?;

    let internal_config = config::Internal::default();
    let cancel_token = CancellationToken::new();

    let metrics_registry = Registry::default();

    let (fuel_adapter, fuel_health_check) =
        setup::fuel_adapter(&config, &internal_config, &metrics_registry);

    let (ethereum_rpc, eth_health_check) =
        setup::l1_adapter(&config, &internal_config, &metrics_registry).await?;

    let commit_interval = ethereum_rpc.commit_interval();

    let wallet_balance_tracker_handle = setup::wallet_balance_tracker(
        &internal_config,
        &metrics_registry,
        ethereum_rpc.clone(),
        cancel_token.clone(),
    );

    let committer_handle = setup::block_committer(
        commit_interval,
        ethereum_rpc.clone(),
        storage.clone(),
        fuel_adapter.clone(),
        &config,
        &metrics_registry,
        cancel_token.clone(),
    );

    let listener_handle = setup::l1_event_listener(
        &internal_config,
        ethereum_rpc.clone(),
        storage.clone(),
        &metrics_registry,
        cancel_token.clone(),
    );

    let mut handles = vec![
        wallet_balance_tracker_handle,
        committer_handle,
        listener_handle,
    ];

    // If the blob pool wallet key is set, we need to start the state committer and state importer
    if config.eth.blob_pool_wallet_key.is_some() {
        let state_committer_handle = setup::state_committer(
            ethereum_rpc,
            storage.clone(),
            &metrics_registry,
            cancel_token.clone(),
            &config,
        );

        let state_importer_handle = setup::state_importer(
            fuel_adapter,
            storage.clone(),
            &metrics_registry,
            cancel_token.clone(),
            &config,
        );

        handles.push(state_committer_handle);
        handles.push(state_importer_handle);
    }

    launch_api_server(
        &config,
        metrics_registry,
        storage.clone(),
        fuel_health_check,
        eth_health_check,
    )
    .await?;

    shut_down(cancel_token, handles, storage).await
}

#[cfg(test)]
mod tests {
    // used in the harness
    use anyhow as _;
}
