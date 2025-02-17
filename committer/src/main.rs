#![deny(unused_crate_dependencies)]
mod api;
mod config;
mod errors;
mod setup;

use api::launch_api_server;
use eigenda::EigenDAClient;
use errors::{Result, WithContext};
use metrics::prometheus::Registry;
use setup::last_finalization_metric;
use tokio_util::sync::CancellationToken;

use crate::setup::shut_down;

pub type L1 = eth::WebsocketClient;
pub type Database = storage::Postgres;
pub type FuelApi = fuel::HttpClient;
pub type EigenDA = EigenDAClient;

#[tokio::main]
async fn main() -> Result<()> {
    setup::logger();

    let config = config::parse().with_context(|| "failed to parse config")?;
    config
        .validate()
        .with_context(|| "config validation failed")?;

    let metrics_registry = Registry::default();

    let finalization_metric = last_finalization_metric();
    let storage = setup::storage(&config, &metrics_registry, &finalization_metric)
        .await
        .with_context(|| "failed to connect to database")?;

    let internal_config = config::Internal::default();
    let cancel_token = CancellationToken::new();

    let (fuel_adapter, fuel_health_check) =
        setup::fuel_adapter(&config, &internal_config, &metrics_registry);

    let (ethereum_rpc, eth_health_check) =
        setup::l1_adapter(&config, &internal_config, &metrics_registry)
            .await
            .with_context(|| "could not setup l1 adapter")?;

    let wallet_balance_tracker_handle = setup::wallet_balance_tracker(
        &internal_config,
        &metrics_registry,
        ethereum_rpc.clone(),
        cancel_token.clone(),
    );

    let committer_handle = setup::block_committer(
        ethereum_rpc.clone(),
        storage.clone(),
        fuel_adapter.clone(),
        &config,
        cancel_token.clone(),
    );

    let mut handles = vec![wallet_balance_tracker_handle, committer_handle];

    // If the blob pool wallet key is set, we need to start
    // the state committer and state importer
    let da_handles = if config.eth.l1_keys.blob.is_some() {
        setup::ethereum_da_services(
            fuel_adapter,
            ethereum_rpc,
            storage.clone(),
            cancel_token.clone(),
            &config,
            &internal_config,
            &metrics_registry,
        )?
    } else if config.da_layer.is_some() {
        setup::eigen_da_services(
            fuel_adapter,
            storage.clone(),
            cancel_token.clone(),
            &config,
            &internal_config,
            &metrics_registry,
        )
        .await?
    } else {
        vec![]
    };

    handles.extend(da_handles);

    launch_api_server(
        &config,
        &internal_config,
        metrics_registry,
        storage.clone(),
        fuel_health_check,
        eth_health_check,
    )
    .await
    .with_context(|| "api server")?;

    shut_down(cancel_token, handles, storage).await
}

#[cfg(test)]
mod tests {
    // used in the harness
    use anyhow as _;
}
