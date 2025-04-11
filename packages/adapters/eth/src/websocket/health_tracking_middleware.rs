use std::{num::NonZeroU32, ops::RangeInclusive};

use ::metrics::{
    ConnectionHealthTracker, HealthChecker, RegistersMetrics, prometheus::core::Collector,
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
        previous_tx: Option<services::types::L1Tx>,
        priority: Priority,
    ) -> Result<(services::types::L1Tx, services::types::FragmentsSubmitted)>;
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
            Err(Error::Network { recoverable, .. }) => {
                if !*recoverable {
                    self.health_tracker.note_permanent_failure();
                } else {
                    self.health_tracker.note_failure();
                }
                self.metrics.eth_network_errors.inc();
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
        previous: Option<services::types::L1Tx>,
        priority: Priority,
    ) -> Result<(services::types::L1Tx, services::types::FragmentsSubmitted)> {
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
    use super::*;
    use ::metrics::prometheus::{Registry, proto::Metric};

    #[tokio::test]
    async fn recoverable_error_does_not_immediately_make_unhealthy() {
        // given: an adapter whose submit method returns a recoverable network error
        // and a threshold of 2 (so one error does not cross the limit).
        let mut eth_adapter = MockEthApi::new();
        eth_adapter.expect_submit().returning(|_, _| {
            Err(Error::Network {
                msg: "Recoverable error".into(),
                recoverable: true,
            })
        });
        let adapter = HealthTrackingMiddleware::new(eth_adapter, 2);
        let health_check = adapter.connection_health_checker();

        // when: a single submit call is made that returns a recoverable error.
        let _ = adapter.submit([0; 32], 0).await;

        // then: the connection should still be healthy (failure count 1 is less than threshold 2).
        assert!(health_check.healthy());
    }

    #[tokio::test]
    async fn successful_call_resets_recoverable_error() {
        // given: an adapter that first returns a recoverable error on submit (threshold = 1)
        // and then returns success on get_block_number.
        let mut eth_adapter = MockEthApi::new();
        eth_adapter.expect_submit().returning(|_, _| {
            Err(Error::Network {
                msg: "Recoverable error".into(),
                recoverable: true,
            })
        });
        eth_adapter
            .expect_get_block_number()
            .returning(|| Ok(10u32.into()));
        let adapter = HealthTrackingMiddleware::new(eth_adapter, 1);
        let health_check = adapter.connection_health_checker();

        // given (pre-condition): Trigger a recoverable error to mark the connection unhealthy.
        let _ = adapter.submit([0; 32], 0).await;
        assert!(!health_check.healthy());

        // when: a single successful get_block_number call is made.
        let _ = adapter.get_block_number().await;

        // then: the connection health should be reset to healthy.
        assert!(health_check.healthy());
    }

    #[tokio::test]
    async fn permanent_error_makes_connection_unhealthy() {
        // given: an adapter whose submit method returns a permanent (non-recoverable) error.
        let mut eth_adapter = MockEthApi::new();
        eth_adapter.expect_submit().returning(|_, _| {
            Err(Error::Network {
                msg: "Permanent error".into(),
                recoverable: false,
            })
        });
        let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
        let health_check = adapter.connection_health_checker();

        // when: a single submit call is made that returns a permanent error.
        let _ = adapter.submit([0; 32], 0).await;

        // then: the connection should be unhealthy.
        assert!(!health_check.healthy());
    }

    #[tokio::test]
    async fn subsequent_success_does_not_reset_permanent_failure() {
        // given: an adapter whose submit method returns a permanent error
        // and whose get_block_number method returns success.
        let mut eth_adapter = MockEthApi::new();
        eth_adapter.expect_submit().returning(|_, _| {
            Err(Error::Network {
                msg: "Permanent error".into(),
                recoverable: false,
            })
        });
        eth_adapter
            .expect_get_block_number()
            .returning(|| Ok(42u32.into()));
        let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
        let health_check = adapter.connection_health_checker();

        // given (pre-condition): Trigger a permanent error.
        let _ = adapter.submit([0; 32], 0).await;
        assert!(!health_check.healthy());

        // when: a single successful get_block_number call is made.
        let _ = adapter.get_block_number().await;

        // then: the connection remains unhealthy because the permanent failure flag is set.
        assert!(!health_check.healthy());
    }

    #[tokio::test]
    async fn other_error_does_not_affect_health_submit() {
        // given: an adapter whose submit method returns a non-network error.
        let mut eth_adapter = MockEthApi::new();
        eth_adapter
            .expect_submit()
            .returning(|_, _| Err(Error::Other("Some error".into())));
        let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
        let health_check = adapter.connection_health_checker();

        // when: a single submit call is made that returns a non-network error.
        let _ = adapter.submit([0; 32], 0).await;

        // then: the connection health remains healthy.
        assert!(health_check.healthy());
    }

    #[tokio::test]
    async fn submit_network_error_increments_metrics() {
        // given: an adapter whose submit method returns a recoverable network error
        let mut eth_adapter = MockEthApi::new();
        eth_adapter.expect_submit().returning(|_, _| {
            Err(Error::Network {
                msg: "Recoverable error".into(),
                recoverable: true,
            })
        });
        let registry = Registry::new();
        let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
        adapter.register_metrics(&registry);

        // when: a single submit call is made that returns a network error.
        let _ = adapter.submit([0; 32], 0).await;

        // then: the "eth_network_errors" metric should be incremented to 1.
        let metrics = registry.gather();
        let counter = metrics
            .iter()
            .find(|m| m.get_name() == "eth_network_errors")
            .and_then(|m| m.get_metric().first())
            .map(Metric::get_counter)
            .unwrap();
        assert_eq!(counter.get_value(), 1f64);
    }
}
