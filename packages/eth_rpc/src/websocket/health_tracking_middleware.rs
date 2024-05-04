use metrics::{ConnectionHealthTracker, HealthChecker, RegistersMetrics};
use ports::{FuelBlock, U256};

use crate::{metrics::Metrics, websocket::event_streamer::EthEventStreamer, Error, Result};

#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait MyAdapter {
    async fn submit(&self, block: FuelBlock) -> Result<()>;
    async fn get_block_number(&self) -> Result<u64>;
    async fn balance(&self) -> Result<U256>;
    fn event_streamer(&self, eth_block_height: u64) -> EthEventStreamer;
    async fn finalized(&self, block: FuelBlock) -> Result<bool>;
    async fn block_hash_at_commit_height(&self, commit_height: u32) -> Result<[u8; 32]>;
}

#[derive(Clone)]
pub struct HealthTrackingMiddleware<T> {
    adapter: T,
    metrics: Metrics,
    health_tracker: ConnectionHealthTracker,
}

impl<T> HealthTrackingMiddleware<T> {
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
        match response {
            Ok(_val) => {
                self.health_tracker.note_success();
            }
            Err(Error::Network(..)) => {
                self.metrics.eth_network_errors.inc();
                self.health_tracker.note_failure();
            }
            _ => {}
        };
    }
}

// User responsible for registering any metrics T might have
impl<T> RegistersMetrics for HealthTrackingMiddleware<T> {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        self.metrics.metrics()
    }
}

#[async_trait::async_trait]
impl<T> MyAdapter for HealthTrackingMiddleware<T>
where
    T: MyAdapter + Send + Sync,
{
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

    fn event_streamer(&self, eth_block_height: u64) -> EthEventStreamer {
        self.adapter.event_streamer(eth_block_height)
    }

    async fn balance(&self) -> Result<U256> {
        let response = self.adapter.balance().await;
        self.note_network_status(&response);
        response
    }

    async fn finalized(&self, block: FuelBlock) -> Result<bool> {
        self.adapter.finalized(block).await
    }

    async fn block_hash_at_commit_height(&self, commit_height: u32) -> Result<[u8; 32]> {
        self.adapter
            .block_hash_at_commit_height(commit_height)
            .await
    }
}

#[cfg(test)]
mod tests {
    use prometheus::{proto::Metric, Registry};

    use super::*;

    #[tokio::test]
    async fn recovers_after_successful_network_request() {
        // given
        let mut eth_adapter = MockMyAdapter::new();
        eth_adapter
            .expect_submit()
            .returning(|_| Err(Error::Network("An error".into())));

        eth_adapter
            .expect_get_block_number()
            .returning(|| Ok(10u32.into()));

        let adapter = HealthTrackingMiddleware::new(eth_adapter, 1);
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
        let mut eth_adapter = MockMyAdapter::new();
        eth_adapter
            .expect_submit()
            .returning(|_| Err(Error::Other("An error".into())));

        eth_adapter
            .expect_get_block_number()
            .returning(|| Err(Error::Other("An error".into())));

        let adapter = HealthTrackingMiddleware::new(eth_adapter, 2);
        let health_check = adapter.connection_health_checker();

        let _ = adapter.submit(given_a_block(42)).await;

        // when
        let _ = adapter.get_block_number().await;

        // then
        assert!(health_check.healthy());
    }

    #[tokio::test]
    async fn network_errors_impact_health_status() {
        let mut eth_adapter = MockMyAdapter::new();
        eth_adapter
            .expect_submit()
            .returning(|_| Err(Error::Network("An error".into())));

        eth_adapter
            .expect_get_block_number()
            .returning(|| Err(Error::Network("An error".into())));

        let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
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
        let mut eth_adapter = MockMyAdapter::new();
        eth_adapter
            .expect_submit()
            .returning(|_| Err(Error::Network("An error".into())));

        eth_adapter
            .expect_get_block_number()
            .returning(|| Err(Error::Network("An error".into())));

        let registry = Registry::new();
        let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
        adapter.register_metrics(&registry);

        let _ = adapter.submit(given_a_block(42)).await;
        let _ = adapter.get_block_number().await;

        let metrics = registry.gather();
        let eth_network_err_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "eth_network_errors")
            .and_then(|metric| metric.get_metric().first())
            .map(Metric::get_counter)
            .unwrap();

        assert_eq!(eth_network_err_metric.get_value(), 2f64);
    }

    fn given_a_block(block_height: u32) -> FuelBlock {
        FuelBlock {
            hash: [0; 32],
            height: block_height,
        }
    }
}
