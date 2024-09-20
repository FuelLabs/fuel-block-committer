use std::ops::RangeInclusive;

#[cfg(feature = "test-helpers")]
use fuel_core_client::client::types::{
    primitives::{Address, AssetId},
    Coin, CoinType,
};
use fuel_core_client::client::{
    pagination::{PageDirection, PaginationRequest},
    types::Block,
    FuelClient as GqlClient,
};
#[cfg(feature = "test-helpers")]
use fuel_core_types::fuel_tx::Transaction;
use futures::{stream, Stream, StreamExt};
use metrics::{
    prometheus::core::Collector, ConnectionHealthTracker, HealthChecker, RegistersMetrics,
};
use url::Url;

use crate::{metrics::Metrics, Error, Result};

mod block_ext;

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

    // TODO: check if this method can be removed
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

    fn create_blocks_request(range: RangeInclusive<u32>) -> Result<PaginationRequest<String>> {
        let start = range.start().saturating_sub(1);
        let results = range
            .end()
            .saturating_sub(*range.start())
            .try_into()
            .map_err(|_| {
                Error::Other(
                    "could not convert `u32` to `i32` when calculating blocks request range"
                        .to_string(),
                )
            })?;

        Ok(PaginationRequest {
            cursor: Some(start.to_string()),
            results,
            direction: PageDirection::Forward,
        })
    }

    pub(crate) fn _block_in_height_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> impl Stream<Item = Result<Vec<Block>>> + '_ {
        let num_blocks_in_request = 100; // TODO: @hal3e make this configurable
        let windowed_range = WindowRangeInclusive::new(range, num_blocks_in_request);

        stream::iter(windowed_range)
            .map(move |range| async move {
                let request = Self::create_blocks_request(range)?;

                Ok(self
                    .client
                    .blocks(request)
                    .await
                    .map_err(|e| Error::Network(e.to_string()))?
                    .results)
            })
            .buffered(2) // TODO: @segfault make this configurable
    }

    pub async fn latest_block(&self) -> Result<Block> {
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

/// An iterator that yields windows of a specified size over a given range.
struct WindowRangeInclusive {
    current: u32,
    end: u32,
    window_size: u32,
}

impl WindowRangeInclusive {
    pub fn new(range: RangeInclusive<u32>, window_size: u32) -> Self {
        Self {
            current: *range.start(),
            end: *range.end(),
            window_size,
        }
    }
}

impl Iterator for WindowRangeInclusive {
    type Item = RangeInclusive<u32>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current > self.end {
            return None;
        }

        let window_end = self.current + self.window_size - 1;
        let window_end = if window_end > self.end {
            self.end
        } else {
            window_end
        };

        let result = self.current..=window_end;
        self.current = window_end + 1;
        Some(result)
    }
}
