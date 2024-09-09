use std::ops::Range;

#[cfg(feature = "test-helpers")]
use fuel_core_client::client::types::{
    primitives::{Address, AssetId},
    Coin, CoinType,
};
use fuel_core_client::client::{types::Block, FuelClient as GqlClient};
#[cfg(feature = "test-helpers")]
use fuel_core_types::fuel_tx::Transaction;
use futures::{stream, Stream, StreamExt};
use metrics::{
    prometheus::core::Collector, ConnectionHealthTracker, HealthChecker, RegistersMetrics,
};
use ports::fuel::BoxStream;
use url::Url;

use crate::{metrics::Metrics, Error, Result};

#[derive(Clone)]
pub struct HttpClient {
    client: GqlClient,
    metrics: Metrics,
    health_tracker: ConnectionHealthTracker,
}

impl HttpClient {
    #[must_use]
    pub fn new(url: &Url, unhealthy_after_n_errors: usize) -> Self {
        let client = GqlClient::new(url).expect("Url to be well formed");
        Self {
            client,
            metrics: Metrics::default(),
            health_tracker: ConnectionHealthTracker::new(unhealthy_after_n_errors),
        }
    }

    #[cfg(feature = "test-helpers")]
    pub async fn produce_blocks(&self, num: u32) -> Result<()> {
        self.client
            .produce_blocks(num, None)
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        Ok(())
    }

    #[cfg(feature = "test-helpers")]
    pub async fn send_tx(&self, tx: &Transaction) -> Result<()> {
        self.client
            .submit_and_await_commit(tx)
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        Ok(())
    }

    #[cfg(feature = "test-helpers")]
    pub async fn get_coin(&self, address: Address, asset_id: AssetId) -> Result<Coin> {
        let coin_type = self
            .client
            .coins_to_spend(&address, vec![(asset_id, 1, None)], None)
            .await
            .map_err(|e| Error::Network(e.to_string()))?[0][0];

        let coin = match coin_type {
            CoinType::Coin(c) => Ok(c),
            _ => Err(Error::Other("Couldn't get coin".to_string())),
        }?;

        Ok(coin)
    }

    #[cfg(feature = "test-helpers")]
    pub async fn health(&self) -> Result<bool> {
        match self.client.health().await {
            Ok(healthy) => {
                self.handle_network_success();
                Ok(healthy)
            }
            Err(err) => {
                self.handle_network_error();
                Err(Error::Network(err.to_string()))
            }
        }
    }

    pub(crate) async fn _block_at_height(&self, height: u32) -> Result<Option<Block>> {
        match self.client.block_by_height(height.into()).await {
            Ok(maybe_block) => {
                self.handle_network_success();
                Ok(maybe_block.map(Into::into))
            }
            Err(err) => {
                self.handle_network_error();
                Err(Error::Network(err.to_string()))
            }
        }
    }

    pub(crate) fn _block_in_height_range(
        &self,
        range: Range<u32>,
    ) -> impl Stream<Item = Result<Block>> + '_ {
        // TODO: segfault make 5 configurable
        stream::iter(range)
            .map(move |height| self._block_at_height(height))
            .buffered(5)
            .filter_map(|result| async move { result.transpose() })
    }

    pub(crate) async fn _latest_block(&self) -> Result<Block> {
        match self.client.chain_info().await {
            Ok(chain_info) => {
                self.handle_network_success();
                Ok(chain_info.latest_block)
            }
            Err(err) => {
                self.handle_network_error();
                Err(Error::Network(err.to_string()))
            }
        }
    }

    #[must_use]
    pub fn connection_health_checker(&self) -> HealthChecker {
        self.health_tracker.tracker()
    }

    fn handle_network_error(&self) {
        self.health_tracker.note_failure();
        self.metrics.fuel_network_errors.inc();
    }

    fn handle_network_success(&self) {
        self.health_tracker.note_success();
    }
}

impl RegistersMetrics for HttpClient {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        self.metrics.metrics()
    }
}
