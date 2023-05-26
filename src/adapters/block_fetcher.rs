use actix_web::dev::Url;
use async_trait::async_trait;
use fuels::{prelude::Provider, types::block::Block};
use prometheus::{IntCounter, Opts, Registry};

use crate::{
    errors::{Error, Result},
    metrics::RegistersMetrics,
};

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait BlockFetcher {
    async fn latest_block(&self) -> Result<Block>;
}

struct Metrics {
    fuel_network_errors: IntCounter,
}

impl Metrics {
    fn default() -> Self {
        let fuel_network_errors = IntCounter::with_opts(Opts::new(
            "fuel_network_errors",
            "Number of network errors encountered while polling for a new Fuel block.",
        ))
        .unwrap();
        Self {
            fuel_network_errors,
        }
    }
}

impl RegistersMetrics for FuelBlockFetcher {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        vec![Box::new(self.metrics.fuel_network_errors.clone())]
    }
}

pub struct FuelBlockFetcher {
    provider: Provider,
    metrics: Metrics,
}

impl FuelBlockFetcher {
    pub async fn connect(url: &Url) -> Result<Self> {
        Ok(Self {
            provider: Provider::connect(url.uri().to_string())
                .await
                .expect("url should be correctly formed"),
            metrics: Metrics::default(),
        })
    }

    fn update_network_err_count(&self) {
        // metrics being incremented  isn't tested since we can't currently kill a
        // spawned fuel node through the SDK
        self.metrics.fuel_network_errors.inc();
    }
}

#[async_trait::async_trait]
impl BlockFetcher for FuelBlockFetcher {
    async fn latest_block(&self) -> Result<Block> {
        self.provider
            .chain_info()
            .await
            .map_err(|err| match err {
                fuels::prelude::ProviderError::ClientRequestError(err) => {
                    self.update_network_err_count();
                    Error::NetworkError(err.to_string())
                }
            })
            .map(|chain_info| chain_info.latest_block)
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::Uri;
    use fuels::test_helpers::{setup_test_provider, Config};

    use super::*;

    #[tokio::test]
    async fn can_fetch_latest_block() {
        // given
        let node_config = Config {
            manual_blocks_enabled: true,
            ..Config::local_node()
        };

        let (provider, addr) =
            setup_test_provider(vec![], vec![], Some(node_config), Some(Default::default())).await;
        provider.produce_blocks(5, None).await.unwrap();

        let uri = Uri::builder()
            .path_and_query(addr.to_string())
            .build()
            .unwrap();

        let block_fetcher = FuelBlockFetcher::connect(&Url::new(uri)).await.unwrap();

        // when
        let result = block_fetcher.latest_block().await.unwrap();

        // then
        assert_eq!(result.header.height, 5);
    }
}
