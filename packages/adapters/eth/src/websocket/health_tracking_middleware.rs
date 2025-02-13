use std::{num::NonZeroU32, ops::RangeInclusive};

use ::metrics::{
    prometheus::core::Collector, ConnectionHealthTracker, HealthChecker, RegistersMetrics,
};
use alloy::rpc::types::FeeHistory;
use delegate::delegate;
use services::{
    state_committer::port::l1::Priority,
    types::{Address, BlockSubmissionTx, Fragment, NonEmpty, TransactionResponse, U256},
};

use crate::{
    error::{Error, Result},
    metrics::Metrics,
};

#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait EthApi {
    async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx>;
    async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
        reward_percentiles: &[f64],
    ) -> Result<FeeHistory>;
    async fn get_block_number(&self) -> Result<u64>;
    async fn balance(&self, address: Address) -> Result<U256>;
    fn commit_interval(&self) -> NonZeroU32;
    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>>;
    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> Result<bool>;
    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<services::types::Fragment>,
        previous_tx: Option<services::types::EthereumDASubmission>,
        priority: Priority,
    ) -> Result<(
        services::types::EthereumDASubmission,
        services::types::FragmentsSubmitted,
    )>;
    #[cfg(feature = "test-helpers")]
    async fn finalized(&self, hash: [u8; 32], height: u32) -> Result<bool>;
    #[cfg(feature = "test-helpers")]
    async fn block_hash_at_commit_height(&self, commit_height: u32) -> Result<[u8; 32]>;
}

#[cfg(test)]
impl RegistersMetrics for MockEthApi {
    fn metrics(&self) -> Vec<Box<dyn metrics::prometheus::core::Collector>> {
        vec![]
    }
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

impl<T: RegistersMetrics> RegistersMetrics for HealthTrackingMiddleware<T> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        self.metrics
            .metrics()
            .into_iter()
            .chain(self.adapter.metrics())
            .collect()
    }
}

#[async_trait::async_trait]
impl<T> EthApi for HealthTrackingMiddleware<T>
where
    T: EthApi + Send + Sync,
{
    delegate! {
        to self.adapter {
            fn commit_interval(&self) -> NonZeroU32;
        }
    }

    async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx> {
        let response = self.adapter.submit(hash, height).await;
        self.note_network_status(&response);
        response
    }

    async fn get_block_number(&self) -> Result<u64> {
        let response = self.adapter.get_block_number().await;
        self.note_network_status(&response);
        response
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>> {
        let response = self.adapter.get_transaction_response(tx_hash).await;
        self.note_network_status(&response);
        response
    }

    async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
        reward_percentiles: &[f64],
    ) -> Result<FeeHistory> {
        let response = self.adapter.fees(height_range, reward_percentiles).await;
        self.note_network_status(&response);
        response
    }

    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> Result<bool> {
        let response = self.adapter.is_squeezed_out(tx_hash).await;
        self.note_network_status(&response);
        response
    }

    async fn balance(&self, address: Address) -> Result<U256> {
        let response = self.adapter.balance(address).await;
        self.note_network_status(&response);
        response
    }

    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous: Option<services::types::EthereumDASubmission>,
        priority: Priority,
    ) -> Result<(
        services::types::EthereumDASubmission,
        services::types::FragmentsSubmitted,
    )> {
        let response = self
            .adapter
            .submit_state_fragments(fragments, previous, priority)
            .await;
        self.note_network_status(&response);
        response
    }

    #[cfg(feature = "test-helpers")]
    async fn finalized(&self, hash: [u8; 32], height: u32) -> Result<bool> {
        let response = self.adapter.finalized(hash, height).await;
        self.note_network_status(&response);
        response
    }

    #[cfg(feature = "test-helpers")]
    async fn block_hash_at_commit_height(&self, commit_height: u32) -> Result<[u8; 32]> {
        let response = self
            .adapter
            .block_hash_at_commit_height(commit_height)
            .await;
        self.note_network_status(&response);
        response
    }
}

#[cfg(test)]
mod tests {
    use ::metrics::prometheus::{proto::Metric, Registry};

    use super::*;

    #[tokio::test]
    async fn recovers_after_successful_network_request() {
        // given
        let mut eth_adapter = MockEthApi::new();
        eth_adapter
            .expect_submit()
            .returning(|_, _| Err(Error::Network("An error".into())));

        eth_adapter
            .expect_get_block_number()
            .returning(|| Ok(10u32.into()));

        let adapter = HealthTrackingMiddleware::new(eth_adapter, 1);
        let health_check = adapter.connection_health_checker();

        let _ = adapter.submit([0; 32], 0).await;

        // when
        let _ = adapter.get_block_number().await;

        // then
        assert!(health_check.healthy());
    }

    #[tokio::test]
    async fn other_errors_dont_impact_health_status() {
        // given
        let mut eth_adapter = MockEthApi::new();
        eth_adapter
            .expect_submit()
            .returning(|_, _| Err(Error::Other("An error".into())));

        eth_adapter
            .expect_get_block_number()
            .returning(|| Err(Error::Other("An error".into())));

        let adapter = HealthTrackingMiddleware::new(eth_adapter, 2);
        let health_check = adapter.connection_health_checker();

        let _ = adapter.submit([0; 32], 0).await;

        // when
        let _ = adapter.get_block_number().await;

        // then
        assert!(health_check.healthy());
    }

    #[tokio::test]
    async fn network_errors_impact_health_status() {
        let mut eth_adapter = MockEthApi::new();
        eth_adapter
            .expect_submit()
            .returning(|_, _| Err(Error::Network("An error".into())));

        eth_adapter
            .expect_get_block_number()
            .returning(|| Err(Error::Network("An error".into())));

        let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
        let health_check = adapter.connection_health_checker();
        assert!(health_check.healthy());

        let _ = adapter.submit([0; 32], 0).await;
        assert!(health_check.healthy());

        let _ = adapter.get_block_number().await;
        assert!(health_check.healthy());

        let _ = adapter.get_block_number().await;
        assert!(!health_check.healthy());
    }

    #[tokio::test]
    async fn network_errors_seen_in_metrics() {
        let mut eth_adapter = MockEthApi::new();
        eth_adapter
            .expect_submit()
            .returning(|_, _| Err(Error::Network("An error".into())));

        eth_adapter
            .expect_get_block_number()
            .returning(|| Err(Error::Network("An error".into())));

        let registry = Registry::new();
        let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
        adapter.register_metrics(&registry);

        let _ = adapter.submit([0; 32], 0).await;
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
}
