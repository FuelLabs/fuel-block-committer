use std::{
    collections::VecDeque,
    fmt::Display,
    num::NonZeroU32,
    ops::RangeInclusive,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use alloy::{primitives::Address, rpc::types::FeeHistory};
use metrics::{HealthCheck, HealthChecker};
use services::{
    state_committer::port::l1::Priority,
    types::{
        BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx, NonEmpty, TransactionResponse, U256,
    },
};
use tokio::sync::Mutex;
use tracing::warn;

use crate::{
    error::{Error as EthError, Result as EthResult},
    provider::L1Provider,
};

#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub name: String,
    pub url: String,
}

impl ProviderConfig {
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
        }
    }
}

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
pub trait ProviderInit: Send + Sync {
    type Provider: L1Provider;

    /// Create a new provider from the given config.
    async fn initialize(&self, config: &ProviderConfig) -> EthResult<Arc<Self::Provider>>;
}

/// Enum to classify error types for failover decisions
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ErrorClassification {
    /// Fatal error that should immediately trigger failover
    Fatal,
    /// Transient error that should trigger failover if threshold is exceeded
    Transient,
    /// Error in the request itself, not related to connection
    Other,
}

/// A client that manages multiple L1 providers and fails over between them
/// when connection issues are detected.
#[derive(Clone)]
pub struct FailoverClient<I>
where
    I: ProviderInit,
{
    // Some object that can create providers of type I::Provider
    initializer: I,
    // Shared mutable state: the provider configs + active provider
    shared_state: Arc<Mutex<SharedState<I::Provider>>>,
    // Maximum number of transient errors before considering a provider unhealthy
    transient_error_threshold: usize,
    /// Exists so that primitive getters don't have to be async and fallable just because they have to
    /// go through the failover logic.
    permanent_cache: PermanentCache,
}

#[derive(Clone)]
struct PermanentCache {
    commit_interval: NonZeroU32,
    contract_caller_address: Address,
    blob_poster_address: Option<Address>,
}

struct SharedState<P> {
    configs: VecDeque<ProviderConfig>,
    active_provider: ProviderHandle<P>,
}

/// Holds an actual provider along with health info (transient errors, etc.)
struct ProviderHandle<P> {
    name: String,
    provider: Arc<P>,
    health: Arc<Health>,
}

impl<P> Clone for ProviderHandle<P> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            provider: Arc::clone(&self.provider),
            health: Arc::clone(&self.health),
        }
    }
}

/// Holds health/tracking info about the provider
struct Health {
    max_transient_errors: usize,
    transient_error_count: AtomicUsize,
    permanently_failed: AtomicBool,
}

impl<P> ProviderHandle<P> {
    fn new(name: String, provider: Arc<P>, max_transient_errors: usize) -> Self {
        let health = Health {
            max_transient_errors,
            transient_error_count: AtomicUsize::new(0),
            permanently_failed: AtomicBool::new(false),
        };
        Self {
            name,
            provider,
            health: Arc::new(health),
        }
    }

    pub fn reset_transient_error_count(&self) {
        self.health
            .transient_error_count
            .store(0, Ordering::Relaxed);
    }

    pub fn note_transient_error(&self, reason: impl Display) {
        let current_count = self
            .health
            .transient_error_count
            .fetch_add(1, Ordering::Relaxed)
            + 1;

        warn!(
            "Transient connection error detected: {reason} \
             (Count: {current_count}/{})",
            self.health.max_transient_errors
        );
    }

    pub fn is_healthy(&self) -> bool {
        // If the provider is marked permanently failed, it is not healthy.
        if self.health.permanently_failed.load(Ordering::Relaxed) {
            return false;
        }

        // If the transient error count is below threshold, it's still considered healthy.
        self.health.transient_error_count.load(Ordering::Relaxed) < self.health.max_transient_errors
    }

    pub fn note_permanent_failure(&self, reason: impl Display) {
        warn!("Provider '{}' permanently failed: {reason}", self.name);
        self.health
            .permanently_failed
            .store(true, Ordering::Relaxed);
    }
}

impl<I> FailoverClient<I>
where
    I: ProviderInit,
{
    /// Create a new FailoverClient with the given provider configurations and initializer.
    /// This attempts to connect to the first available provider and stores both
    /// the `configs` and the `active_provider` together in `SharedState`.
    pub async fn connect(
        provider_configs: Vec<ProviderConfig>,
        initializer: I,
        transient_error_threshold: usize,
    ) -> EthResult<Self> {
        let mut configs = VecDeque::from(provider_configs);

        // Attempt to connect right away
        let provider_handle = connect_to_first_available_provider(
            &initializer,
            &mut configs,
            transient_error_threshold,
        )
        .await?;

        let commit_interval = provider_handle.provider.commit_interval();
        let contract_caller_address = provider_handle.provider.contract_caller_address();
        let blob_poster_address = provider_handle.provider.blob_poster_address();

        // Wrap it all in a single Mutex
        let shared_state = SharedState {
            configs,
            active_provider: provider_handle,
        };

        Ok(Self {
            initializer,
            shared_state: Arc::new(Mutex::new(shared_state)),
            transient_error_threshold,
            permanent_cache: PermanentCache {
                commit_interval,
                contract_caller_address,
                blob_poster_address,
            },
        })
    }

    pub fn health_checker(&self) -> HealthChecker
    where
        Self: Clone + 'static,
    {
        Box::new(self.clone())
    }

    /// Tries to classify the error for failover logic
    fn classify_error(err: &EthError) -> ErrorClassification {
        match err {
            // Fatal errors that should immediately trigger failover
            EthError::Network {
                recoverable: false, ..
            } => ErrorClassification::Fatal,

            // Transient errors that should only trigger failover after threshold is exceeded
            EthError::Network {
                recoverable: true, ..
            } => ErrorClassification::Transient,

            // Request-specific errors that shouldn't trigger failover
            EthError::TxExecution(_) | EthError::Other(_) => ErrorClassification::Other,
        }
    }

    /// Retrieves the currently active provider if it's healthy, or tries to fail over to the next.
    async fn get_healthy_provider(&self) -> EthResult<ProviderHandle<I::Provider>> {
        let mut state = self.shared_state.lock().await;

        // Check if the active provider is healthy
        if state.active_provider.is_healthy() {
            return Ok(state.active_provider.clone());
        }

        let handle = connect_to_first_available_provider(
            &self.initializer,
            &mut state.configs,
            self.transient_error_threshold,
        )
        .await?;

        state.active_provider = handle.clone();
        Ok(handle)
    }

    pub async fn make_current_connection_permanently_failed(
        &self,
        reason: String,
    ) -> EthResult<()> {
        self.shared_state
            .lock()
            .await
            .active_provider
            .note_permanent_failure(reason);
        Ok(())
    }

    /// Core abstraction to execute operations with failover logic
    async fn execute_operation<F, Fut, T>(&self, operation_factory: F) -> EthResult<T>
    where
        F: FnOnce(Arc<I::Provider>) -> Fut + Send + Sync,
        Fut: std::future::Future<Output = EthResult<T>> + Send,
        T: Send,
    {
        let provider = self.get_healthy_provider().await?;

        let result = operation_factory(Arc::clone(&provider.provider)).await;

        match result {
            Ok(value) => {
                provider.reset_transient_error_count();
                Ok(value)
            }
            Err(error) => match Self::classify_error(&error) {
                ErrorClassification::Fatal => {
                    provider.note_permanent_failure(&error);
                    Err(error)
                }
                ErrorClassification::Transient => {
                    provider.note_transient_error(&error);
                    Err(error)
                }
                ErrorClassification::Other => Err(error),
            },
        }
    }
}

impl<I> L1Provider for FailoverClient<I>
where
    I: ProviderInit,
{
    async fn get_block_number(&self) -> EthResult<u64> {
        self.execute_operation(|provider| async move { provider.get_block_number().await })
            .await
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> EthResult<Option<TransactionResponse>> {
        self.execute_operation(move |provider| async move {
            provider.get_transaction_response(tx_hash).await
        })
        .await
    }

    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> EthResult<bool> {
        self.execute_operation(
            move |provider| async move { provider.is_squeezed_out(tx_hash).await },
        )
        .await
    }

    async fn balance(&self, address: Address) -> EthResult<U256> {
        self.execute_operation(move |provider| async move { provider.balance(address).await })
            .await
    }

    async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
        reward_percentiles: &[f64],
    ) -> EthResult<FeeHistory> {
        self.execute_operation(move |provider| async move {
            provider
                .fees(height_range.clone(), reward_percentiles)
                .await
        })
        .await
    }

    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<L1Tx>,
        priority: Priority,
    ) -> EthResult<(L1Tx, FragmentsSubmitted)> {
        self.execute_operation(move |provider| async move {
            provider
                .submit_state_fragments(fragments.clone(), previous_tx.clone(), priority)
                .await
        })
        .await
    }

    async fn submit(&self, hash: [u8; 32], height: u32) -> EthResult<BlockSubmissionTx> {
        self.execute_operation(move |provider| async move { provider.submit(hash, height).await })
            .await
    }

    fn commit_interval(&self) -> NonZeroU32 {
        self.permanent_cache.commit_interval
    }

    fn blob_poster_address(&self) -> Option<Address> {
        self.permanent_cache.blob_poster_address
    }

    fn contract_caller_address(&self) -> Address {
        self.permanent_cache.contract_caller_address
    }
}

/// Attempts to connect to the first available provider, removing each config from
/// the front of the deque if it fails.
async fn connect_to_first_available_provider<I: ProviderInit>(
    initializer: &I,
    configs: &mut VecDeque<ProviderConfig>,
    transient_error_threshold: usize,
) -> EthResult<ProviderHandle<I::Provider>> {
    while let Some(config) = configs.pop_front() {
        match initializer.initialize(&config).await {
            Ok(provider) => {
                let tracked = ProviderHandle::new(config.name, provider, transient_error_threshold);
                return Ok(tracked);
            }
            Err(err) => {
                warn!(
                    "Could not fail over to remote provider {}: {err}",
                    config.name
                );
            }
        }
    }

    Err(EthError::Other("no more providers available".into()))
}

use ::metrics::{
    RegistersMetrics,
    prometheus::{IntCounter, Opts, core::Collector},
};

#[derive(Clone)]
pub struct Metrics {
    pub(crate) eth_network_errors: IntCounter,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
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

#[async_trait::async_trait]
impl<I> HealthCheck for FailoverClient<I>
where
    I: ProviderInit,
{
    async fn healthy(&self) -> bool {
        self.get_healthy_provider().await.is_ok()
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use ::metrics::prometheus::{Registry, proto::Metric};
//
//     #[tokio::test]
//     async fn recoverable_error_does_not_immediately_make_unhealthy() {
//         // given: an adapter whose submit method returns a recoverable network error
//         // and a threshold of 2 (so one error does not cross the limit).
//         let mut eth_adapter = MockEthApi::new();
//         eth_adapter.expect_submit().returning(|_, _| {
//             Err(Error::Network {
//                 msg: "Recoverable error".into(),
//                 recoverable: true,
//             })
//         });
//         let adapter = HealthTrackingMiddleware::new(eth_adapter, 2);
//         let health_check = adapter.connection_health_checker();
//
//         // when: a single submit call is made that returns a recoverable error.
//         let _ = adapter.submit([0; 32], 0).await;
//
//         // then: the connection should still be healthy (failure count 1 is less than threshold 2).
//         assert!(health_check.healthy());
//     }
//
//     #[tokio::test]
//     async fn successful_call_resets_recoverable_error() {
//         // given: an adapter that first returns a recoverable error on submit (threshold = 1)
//         // and then returns success on get_block_number.
//         let mut eth_adapter = MockEthApi::new();
//         eth_adapter.expect_submit().returning(|_, _| {
//             Err(Error::Network {
//                 msg: "Recoverable error".into(),
//                 recoverable: true,
//             })
//         });
//         eth_adapter
//             .expect_get_block_number()
//             .returning(|| Ok(10u32.into()));
//         let adapter = HealthTrackingMiddleware::new(eth_adapter, 1);
//         let health_check = adapter.connection_health_checker();
//
//         // given (pre-condition): Trigger a recoverable error to mark the connection unhealthy.
//         let _ = adapter.submit([0; 32], 0).await;
//         assert!(!health_check.healthy());
//
//         // when: a single successful get_block_number call is made.
//         let _ = adapter.get_block_number().await;
//
//         // then: the connection health should be reset to healthy.
//         assert!(health_check.healthy());
//     }
//
//     #[tokio::test]
//     async fn permanent_error_makes_connection_unhealthy() {
//         // given: an adapter whose submit method returns a permanent (non-recoverable) error.
//         let mut eth_adapter = MockEthApi::new();
//         eth_adapter.expect_submit().returning(|_, _| {
//             Err(Error::Network {
//                 msg: "Permanent error".into(),
//                 recoverable: false,
//             })
//         });
//         let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
//         let health_check = adapter.connection_health_checker();
//
//         // when: a single submit call is made that returns a permanent error.
//         let _ = adapter.submit([0; 32], 0).await;
//
//         // then: the connection should be unhealthy.
//         assert!(!health_check.healthy());
//     }
//
//     #[tokio::test]
//     async fn subsequent_success_does_not_reset_permanent_failure() {
//         // given: an adapter whose submit method returns a permanent error
//         // and whose get_block_number method returns success.
//         let mut eth_adapter = MockEthApi::new();
//         eth_adapter.expect_submit().returning(|_, _| {
//             Err(Error::Network {
//                 msg: "Permanent error".into(),
//                 recoverable: false,
//             })
//         });
//         eth_adapter
//             .expect_get_block_number()
//             .returning(|| Ok(42u32.into()));
//         let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
//         let health_check = adapter.connection_health_checker();
//
//         // given (pre-condition): Trigger a permanent error.
//         let _ = adapter.submit([0; 32], 0).await;
//         assert!(!health_check.healthy());
//
//         // when: a single successful get_block_number call is made.
//         let _ = adapter.get_block_number().await;
//
//         // then: the connection remains unhealthy because the permanent failure flag is set.
//         assert!(!health_check.healthy());
//     }
//
//     #[tokio::test]
//     async fn other_error_does_not_affect_health_submit() {
//         // given: an adapter whose submit method returns a non-network error.
//         let mut eth_adapter = MockEthApi::new();
//         eth_adapter
//             .expect_submit()
//             .returning(|_, _| Err(Error::Other("Some error".into())));
//         let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
//         let health_check = adapter.connection_health_checker();
//
//         // when: a single submit call is made that returns a non-network error.
//         let _ = adapter.submit([0; 32], 0).await;
//
//         // then: the connection health remains healthy.
//         assert!(health_check.healthy());
//     }
//
//     #[tokio::test]
//     async fn submit_network_error_increments_metrics() {
//         // given: an adapter whose submit method returns a recoverable network error
//         let mut eth_adapter = MockEthApi::new();
//         eth_adapter.expect_submit().returning(|_, _| {
//             Err(Error::Network {
//                 msg: "Recoverable error".into(),
//                 recoverable: true,
//             })
//         });
//         let registry = Registry::new();
//         let adapter = HealthTrackingMiddleware::new(eth_adapter, 3);
//         adapter.register_metrics(&registry);
//
//         // when: a single submit call is made that returns a network error.
//         let _ = adapter.submit([0; 32], 0).await;
//
//         // then: the "eth_network_errors" metric should be incremented to 1.
//         let metrics = registry.gather();
//         let counter = metrics
//             .iter()
//             .find(|m| m.get_name() == "eth_network_errors")
//             .and_then(|m| m.get_metric().first())
//             .map(Metric::get_counter)
//             .unwrap();
//         assert_eq!(counter.get_value(), 1f64);
//     }
// }
