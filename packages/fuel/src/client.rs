use std::{cmp::min, ops::RangeInclusive};

use block_ext::{ClientExt, FullBlock};
#[cfg(feature = "test-helpers")]
use fuel_core_client::client::types::{
    primitives::{Address, AssetId},
    Coin, CoinType,
};
use fuel_core_client::client::{
    pagination::{PageDirection, PaginatedResult, PaginationRequest},
    types::Block,
    FuelClient as GqlClient,
};
#[cfg(feature = "test-helpers")]
use fuel_core_types::fuel_tx::Transaction;
use futures::{stream, Stream};
use metrics::{
    prometheus::core::Collector, ConnectionHealthTracker, HealthChecker, RegistersMetrics,
};
use ports::types::NonEmptyVec;
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
    pub(crate) async fn block_at_height(&self, height: u32) -> Result<Option<Block>> {
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

    pub(crate) fn block_in_height_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> impl Stream<Item = Result<NonEmptyVec<ports::fuel::FullFuelBlock>>> + '_ {
        const MAX_BLOCKS_PER_REQUEST: i32 = 100; // TODO: @hal3e make this configurable

        struct Progress {
            cursor: Option<String>,
            blocks_so_far: usize,
            target_amount: usize,
        }

        impl Progress {
            pub fn new(range: RangeInclusive<u32>) -> Self {
                // Cursor represents the block height of the last block in the previous request.
                let cursor = range.start().checked_sub(1).map(|v| v.to_string());

                Self {
                    cursor,
                    blocks_so_far: 0,
                    target_amount: range.count(),
                }
            }
        }

        impl Progress {
            fn consume(&mut self, result: PaginatedResult<FullBlock, String>) -> Vec<FullBlock> {
                self.blocks_so_far += result.results.len();
                self.cursor = result.cursor;
                result.results
            }

            fn take_cursor(&mut self) -> Option<String> {
                self.cursor.take()
            }

            fn remaining(&self) -> i32 {
                self.target_amount.saturating_sub(self.blocks_so_far) as i32
            }
        }

        let initial_progress = Progress::new(range);

        stream::try_unfold(initial_progress, move |mut current_progress| async move {
            let request = PaginationRequest {
                cursor: current_progress.take_cursor(),
                results: min(current_progress.remaining(), MAX_BLOCKS_PER_REQUEST),
                direction: PageDirection::Forward,
            };

            let response = self
                .client
                .full_blocks(request.clone())
                .await
                .map_err(|e| {
                    Error::Network(format!(
                        "While sending request for full blocks: {request:?} got error: {e}"
                    ))
                })?;

            if response.results.is_empty() {
                return Ok(None);
            }

            let results: Vec<_> = current_progress
                .consume(response)
                .into_iter()
                .map(|b| b.into())
                .collect();

            let results = NonEmptyVec::try_from(results).expect("should be non-empty");

            Ok(Some((results, current_progress)))
        })
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
