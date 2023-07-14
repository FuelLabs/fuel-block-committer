use ethers::types::{H160, U256};
use prometheus::{IntCounter, Opts};

use crate::{
    adapters::{
        ethereum_adapter::{EthereumAdapter, EventStreamer},
        fuel_adapter::FuelBlock,
    },
    errors::{Error, Result},
    telemetry::{ConnectionHealthTracker, HealthChecker, RegistersMetrics},
};

#[derive(Clone)]
pub struct MonitoredEthAdapter<T> {
    adapter: T,
    metrics: Metrics,
    health_tracker: ConnectionHealthTracker,
}

impl<T> MonitoredEthAdapter<T> {
    pub fn new(adapter: T, unhealthy_after_n_errors: usize) -> Self {
        Self {
            adapter,
            metrics: Metrics::default(),
            health_tracker: ConnectionHealthTracker::new(unhealthy_after_n_errors),
        }
    }

    pub fn connection_health_checker(&self) -> HealthChecker {
        self.health_tracker.tracker()
    }

    fn note_network_status<K>(&self, response: &Result<K>) {
        match &response {
            Ok(_val) => {
                self.health_tracker.note_success();
            }
            Err(Error::NetworkError(..)) => {
                self.metrics.eth_network_errors.inc();
                self.health_tracker.note_failure();
            }
            _ => {}
        };
    }
}

// User responsible for registering any metrics T might have
impl<T> RegistersMetrics for MonitoredEthAdapter<T> {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        self.metrics.metrics()
    }
}

#[async_trait::async_trait]
impl<T: EthereumAdapter> EthereumAdapter for MonitoredEthAdapter<T> {
    async fn submit(&self, block: FuelBlock) -> Result<()> {
        let response = self.adapter.submit(block).await;
        self.note_network_status(&response);
        response
    }

    async fn get_block_number(&self) -> Result<u64> {
        let response = self.adapter.get_block_number().await;
        self.note_network_status(&response);
        response
    }

    fn event_streamer(&self, eth_block_height: u64) -> Box<dyn EventStreamer + Send + Sync> {
        self.adapter.event_streamer(eth_block_height)
    }

    async fn balance(&self, address: H160) -> Result<U256> {
        let response = self.adapter.balance(address).await;
        self.note_network_status(&response);
        response
    }
}

#[derive(Clone)]
struct Metrics {
    eth_network_errors: IntCounter,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        vec![Box::new(self.eth_network_errors.clone())]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let eth_network_errors = IntCounter::with_opts(Opts::new(
            "eth_network_errors",
            "Number of network errors encountered while running Ethereum RPCs.",
        ))
        .expect("eth_network_errors metric to be correctly configured");

        Self { eth_network_errors }
    }
}

#[cfg(test)]
mod tests {
    use prometheus::Registry;

    use super::*;
    use crate::adapters::ethereum_adapter::MockEthereumAdapter;

    #[tokio::test]
    async fn recovers_after_successful_network_request() {
        // given
        let mut eth_adapter = MockEthereumAdapter::new();
        eth_adapter
            .expect_submit()
            .returning(|_| Err(Error::NetworkError("An error".into())));

        eth_adapter.expect_get_block_number().returning(|| Ok(10));

        let adapter = MonitoredEthAdapter::new(eth_adapter, 1);
        let health_check = adapter.connection_health_checker();

        let _ = adapter.submit(given_a_block(42)).await;

        // when
        let _ = adapter.get_block_number().await;

        // then
        assert!(health_check.healthy());
    }

    #[tokio::test]
    async fn other_errors_dont_impact_health_status() {
        // given
        let mut eth_adapter = MockEthereumAdapter::new();
        eth_adapter
            .expect_submit()
            .returning(|_| Err(Error::Other("An error".into())));

        eth_adapter
            .expect_get_block_number()
            .returning(|| Err(Error::Other("An error".into())));

        let adapter = MonitoredEthAdapter::new(eth_adapter, 2);
        let health_check = adapter.connection_health_checker();

        let _ = adapter.submit(given_a_block(42)).await;

        // when
        let _ = adapter.get_block_number().await;

        // then
        assert!(health_check.healthy());
    }

    #[tokio::test]
    async fn network_errors_impact_health_status() {
        let mut eth_adapter = MockEthereumAdapter::new();
        eth_adapter
            .expect_submit()
            .returning(|_| Err(Error::NetworkError("An error".into())));

        eth_adapter
            .expect_get_block_number()
            .returning(|| Err(Error::NetworkError("An error".into())));

        let adapter = MonitoredEthAdapter::new(eth_adapter, 3);
        let health_check = adapter.connection_health_checker();
        assert!(health_check.healthy());

        let _ = adapter.submit(given_a_block(42)).await;
        assert!(health_check.healthy());

        let _ = adapter.get_block_number().await;
        assert!(health_check.healthy());

        let _ = adapter.get_block_number().await;
        assert!(!health_check.healthy());
    }

    #[tokio::test]
    async fn network_errors_seen_in_metrics() {
        let mut eth_adapter = MockEthereumAdapter::new();
        eth_adapter
            .expect_submit()
            .returning(|_| Err(Error::NetworkError("An error".into())));

        eth_adapter
            .expect_get_block_number()
            .returning(|| Err(Error::NetworkError("An error".into())));

        let registry = Registry::new();
        let adapter = MonitoredEthAdapter::new(eth_adapter, 3);
        adapter.register_metrics(&registry);

        let _ = adapter.submit(given_a_block(42)).await;
        let _ = adapter.get_block_number().await;

        let metrics = registry.gather();
        let latest_block_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "eth_network_errors")
            .and_then(|metric| metric.get_metric().get(0))
            .map(|metric| metric.get_counter())
            .unwrap();

        assert_eq!(latest_block_metric.get_value(), 2f64);
    }

    fn given_a_block(block_height: u32) -> FuelBlock {
        FuelBlock {
            hash: [0; 32],
            height: block_height,
        }
    }
}
