use fuels::{
    client::FuelClient,
    prelude::{Provider, ProviderError},
    types::block::Block as FuelBlock,
};
use url::Url;

use crate::{
    adapters::block_fetcher::{health_tracker::FuelHealthTracker, metrics::Metrics, BlockFetcher},
    errors::{Error, Result},
    telemetry::{HealthChecker, RegistersMetrics},
};

impl RegistersMetrics for FuelBlockFetcher {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        self.metrics.metrics()
    }
}

pub struct FuelBlockFetcher {
    provider: Provider,
    metrics: Metrics,
    health_tracker: FuelHealthTracker,
}

impl FuelBlockFetcher {
    pub fn new(url: &Url, unhealthy_after_n_errors: usize) -> Self {
        let client = FuelClient::new(url).expect("Url to be well formed");
        let provider = Provider::new(client, Default::default());
        Self {
            provider,
            metrics: Metrics::default(),
            health_tracker: FuelHealthTracker::new(unhealthy_after_n_errors),
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

#[async_trait::async_trait]
impl BlockFetcher for FuelBlockFetcher {
    async fn latest_block(&self) -> Result<FuelBlock> {
        match self.provider.chain_info().await {
            Ok(chain_info) => {
                self.handle_network_success();
                Ok(chain_info.latest_block)
            }
            Err(ProviderError::ClientRequestError(err)) => {
                self.handle_network_error();
                Err(Error::NetworkError(err.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use fuels::test_helpers::{setup_test_provider, Config};
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

        let block_fetcher = FuelBlockFetcher::new(&url, 1);

        // when
        let result = block_fetcher.latest_block().await.unwrap();

        // then
        assert_eq!(result.header.height, 5);
    }

    #[tokio::test]
    async fn updates_metrics_in_case_of_network_err() {
        // temporary 'fake' address to cause a network error the same effect will be achieved by
        // killing the node once the SDK supports it.
        let url = Url::parse("localhost:12344").unwrap();

        let block_fetcher = FuelBlockFetcher::new(&url, 1);

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

        let block_fetcher = FuelBlockFetcher::new(&url, 3);
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
