use fuel_core_client::client::{schema::block::Block as FuelGqlBlock, FuelClient};
use url::Url;

use crate::{
    adapters::block_fetcher::{fuel_metrics::FuelMetrics, FuelAdapter, FuelBlock},
    errors::{Error, Result},
    telemetry::{ConnectionHealthTracker, HealthChecker, RegistersMetrics},
};

impl RegistersMetrics for FuelClientAdapter {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        self.metrics.metrics()
    }
}

pub struct FuelClientAdapter {
    client: FuelClient,
    metrics: FuelMetrics,
    health_tracker: ConnectionHealthTracker,
}

impl FuelClientAdapter {
    pub fn new(url: &Url, unhealthy_after_n_errors: usize) -> Self {
        let client = FuelClient::new(url).expect("Url to be well formed");
        Self {
            client,
            metrics: FuelMetrics::default(),
            health_tracker: ConnectionHealthTracker::new(unhealthy_after_n_errors),
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

impl From<FuelGqlBlock> for FuelBlock {
    fn from(value: FuelGqlBlock) -> Self {
        FuelBlock {
            hash: *value.id.0 .0,
            height: value.header.height.0,
        }
    }
}

#[async_trait::async_trait]
impl FuelAdapter for FuelClientAdapter {
    async fn block_at_height(&self, height: u32) -> Result<Option<FuelBlock>> {
        let maybe_block = self
            .client
            .block_by_height(height as u64)
            .await
            .map_err(|e| Error::NetworkError(e.to_string()))?;

        Ok(maybe_block.map(Into::into))
    }

    async fn latest_block(&self) -> Result<FuelBlock> {
        match self.client.chain_info().await {
            Ok(chain_info) => {
                self.handle_network_success();
                Ok(chain_info.latest_block.into())
            }
            Err(err) => {
                self.handle_network_error();
                Err(Error::NetworkError(err.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use fuels_test_helpers::{setup_test_provider, Config};
    use prometheus::Registry;

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

        let url = Url::parse(&format!("http://{addr}")).unwrap();

        let block_fetcher = FuelClientAdapter::new(&url, 1);

        // when
        let result = block_fetcher.latest_block().await.unwrap();

        // then
        assert_eq!(result.height, 5);
    }

    #[tokio::test]
    async fn can_fetch_block_at_height() {
        // given
        let node_config = Config {
            manual_blocks_enabled: true,
            ..Config::local_node()
        };

        let (provider, addr) =
            setup_test_provider(vec![], vec![], Some(node_config), Some(Default::default())).await;
        provider.produce_blocks(5, None).await.unwrap();

        let url = Url::parse(&format!("http://{addr}")).unwrap();

        let block_fetcher = FuelClientAdapter::new(&url, 1);

        // when
        let result = block_fetcher.block_at_height(3).await.unwrap().unwrap();

        // then
        assert_eq!(result.height, 3);
    }

    #[tokio::test]
    async fn updates_metrics_in_case_of_network_err() {
        // temporary 'fake' address to cause a network error the same effect will be achieved by
        // killing the node once the SDK supports it.
        let url = Url::parse("localhost:12344").unwrap();

        let block_fetcher = FuelClientAdapter::new(&url, 1);

        let registry = Registry::default();
        block_fetcher.register_metrics(&registry);

        // when
        let result = block_fetcher.latest_block().await;

        // then
        assert!(result.is_err());
        let metrics = registry.gather();
        let network_errors_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "fuel_network_errors")
            .and_then(|metric| metric.get_metric().get(0))
            .map(|metric| metric.get_counter())
            .unwrap();

        assert_eq!(network_errors_metric.get_value(), 1f64);
    }

    #[tokio::test]
    async fn correctly_tracks_network_health() {
        // temporary 'fake' address to cause a network error the same effect will be achieved by
        // killing the node once the SDK supports it.
        let url = Url::parse("http://localhost:12344").unwrap();

        let block_fetcher = FuelClientAdapter::new(&url, 3);
        let health_check = block_fetcher.connection_health_checker();

        assert!(health_check.healthy());

        let _ = block_fetcher.latest_block().await;
        assert!(health_check.healthy());

        let _ = block_fetcher.latest_block().await;
        assert!(health_check.healthy());

        let _ = block_fetcher.latest_block().await;
        assert!(!health_check.healthy());
    }
}
