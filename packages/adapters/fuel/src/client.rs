use std::{num::NonZeroU32, ops::RangeInclusive, str::FromStr};

use custom_queries::block_at_height::{BlockAtHeightQuery, BlockAtHeightVariables};
use cynic::QueryBuilder;
#[cfg(feature = "test-helpers")]
use fuel_core_client::client::types::{
    primitives::{Address, AssetId},
    Coin, CoinType,
};
use fuel_core_client::client::FuelClient as GqlClient;
use fuel_core_types::fuel_tx::Bytes32;
#[cfg(feature = "test-helpers")]
use fuel_core_types::fuel_tx::Transaction;
use futures::{stream, Stream, StreamExt};
use metrics::{
    prometheus::core::Collector, ConnectionHealthTracker, HealthChecker, RegistersMetrics,
};
use services::{
    block_committer::port::fuel::FuelBlock,
    types::{CompressedFuelBlock, NonEmpty},
    Error, Result,
};
use url::Url;

use crate::metrics::Metrics;

mod custom_queries {
    #[cynic::schema("fuelcore")]
    mod schema {}

    #[derive(cynic::Scalar, Debug, Clone)]
    pub struct BlockId(pub String);

    #[derive(cynic::Scalar, Debug, Clone)]
    pub struct U32(pub String);

    #[derive(cynic::QueryFragment, Debug)]
    pub struct Block {
        pub height: U32,
        pub id: BlockId,
    }

    pub mod latest_block {
        use super::*;

        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "Query")]
        pub struct LatestBlockQuery {
            pub chain: ChainInfo,
        }

        #[derive(cynic::QueryFragment, Debug)]
        pub struct ChainInfo {
            pub latest_block: Block,
        }
    }

    pub mod block_at_height {
        use super::*;

        #[derive(cynic::QueryVariables, Debug)]
        pub struct BlockAtHeightVariables {
            pub height: U32,
        }

        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "Query", variables = "BlockAtHeightVariables")]
        pub struct BlockAtHeightQuery {
            #[arguments(height: $height)]
            pub block: Option<Block>,
        }
    }
}

#[derive(Clone)]
pub struct HttpClient {
    client: GqlClient,
    metrics: Metrics,
    health_tracker: ConnectionHealthTracker,
    num_buffered_requests: NonZeroU32,
}

impl HttpClient {
    pub fn new(
        url: &Url,
        unhealthy_after_n_errors: usize,
        num_buffered_requests: NonZeroU32,
    ) -> Self {
        let client = GqlClient::new(url).expect("Url to be well formed");
        Self {
            client,
            metrics: Metrics::default(),
            health_tracker: ConnectionHealthTracker::new(unhealthy_after_n_errors),
            num_buffered_requests,
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

    pub(crate) async fn block_at_height(&self, height: u32) -> Result<Option<FuelBlock>> {
        let query = BlockAtHeightQuery::build(BlockAtHeightVariables {
            height: custom_queries::U32(height.to_string()),
        });

        match self.client.query(query).await {
            Ok(maybe_block) => {
                self.handle_network_success();
                let Some(block) = maybe_block.block else {
                    return Ok(None);
                };

                let height = block.height.0.parse().map_err(|e| {
                    Error::Other(format!(
                        "couldn't decode fuel block at height: {height}, invalid height: {e}"
                    ))
                })?;

                let id = *Bytes32::from_str(&block.id.0).map_err(|e| {
                    Error::Other(format!(
                        "couldn't decode fuel block at height: {height}, invalid id: {e}"
                    ))
                })?;

                Ok(Some(FuelBlock { id, height }))
            }
            Err(err) => {
                self.handle_network_error();
                Err(Error::Network(err.to_string()))
            }
        }
    }

    pub(crate) async fn compressed_block_at_height(
        &self,
        height: u32,
    ) -> Result<Option<CompressedFuelBlock>> {
        match self.client.da_compressed_block(height.into()).await {
            Ok(maybe_block) => {
                self.handle_network_success();
                match maybe_block {
                    Some(data) => {
                        let non_empty_data = NonEmpty::collect(data).ok_or_else(|| {
                            Error::Other(format!(
                                "encountered empty compressed block at height: {height}",
                            ))
                        })?;

                        Ok(Some(CompressedFuelBlock {
                            height,
                            data: non_empty_data,
                        }))
                    }
                    None => Err(Error::Other(format!(
                        "compressed block not found at height: {height}",
                    ))),
                }
            }
            Err(err) => {
                self.handle_network_error();
                Err(Error::Network(err.to_string()))
            }
        }
    }

    pub(crate) fn _compressed_blocks_in_height_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> impl Stream<Item = Result<CompressedFuelBlock>> + '_ {
        stream::iter(range)
            .map(move |height| self.compressed_block_at_height(height))
            .buffered(self.num_buffered_requests.get() as usize)
            .filter_map(|result| async move { result.transpose() })
    }

    pub async fn latest_block(&self) -> Result<FuelBlock> {
        let query = custom_queries::latest_block::LatestBlockQuery::build(());

        match self.client.query(query).await {
            Ok(chain_info) => {
                self.handle_network_success();
                let block = chain_info.chain.latest_block;
                let id = *Bytes32::from_str(&block.id.0).map_err(|e| {
                    Error::Other(format!(
                        "couldn't decode latest fuel block, invalid id: {e}"
                    ))
                })?;
                let data = block.height.0.trim_start_matches("0x");
                let height = u32::from_str(data).map_err(|e| {
                    Error::Other(format!(
                        "couldn't decode latest fuel block, invalid height: {e}"
                    ))
                })?;

                self.metrics.fuel_height.set(height.into());

                Ok(FuelBlock { id, height })
            }
            Err(err) => {
                self.handle_network_error();
                Err(Error::Network(err.to_string()))
            }
        }
    }

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
