use std::{
    collections::VecDeque,
    fmt::Display,
    num::NonZeroU32,
    ops::RangeInclusive,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use alloy::{primitives::Address, rpc::types::FeeHistory};
use metrics::{HealthCheck, HealthChecker};
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Endpoint {
    pub name: String,
    pub url: url::Url,
}

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
pub trait ProviderInit: Send + Sync {
    type Provider: L1Provider;

    /// Create a new provider from the given config.
    async fn initialize(&self, config: &Endpoint) -> EthResult<Arc<Self::Provider>>;
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
    // Configuration for transaction failure tracking
    tx_failure_threshold: usize,
    tx_failure_time_window: Duration,
    /// Exists so that primitive getters don't have to be async and fallable just because they have to
    /// go through the failover logic.
    permanent_cache: PermanentCache,
    // Metrics for monitoring
    metrics: Metrics,
}

#[derive(Clone)]
struct PermanentCache {
    commit_interval: NonZeroU32,
    contract_caller_address: Address,
    blob_poster_address: Option<Address>,
}

struct SharedState<P> {
    configs: VecDeque<Endpoint>,
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
    // Track transactions that failed not due to network errors but due to mempool issues
    tx_failure_window: Mutex<VecDeque<Instant>>,
    tx_failure_threshold: usize, // Number of failed transactions to trigger unhealthy state
    tx_failure_time_window: Duration, // Time window to consider for failures
}

impl<P> ProviderHandle<P> {
    fn new(
        name: String,
        provider: Arc<P>,
        max_transient_errors: usize,
        tx_failure_threshold: usize,
        tx_failure_time_window: Duration,
    ) -> Self {
        let health = Health {
            max_transient_errors,
            transient_error_count: AtomicUsize::new(0),
            permanently_failed: AtomicBool::new(false),
            tx_failure_window: Mutex::new(VecDeque::with_capacity(tx_failure_threshold + 1)),
            tx_failure_threshold,
            tx_failure_time_window,
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

    pub async fn note_tx_failure(&self, reason: impl Display) {
        let now = Instant::now();
        let mut failure_window = self.health.tx_failure_window.lock().await;

        // Remove old entries outside the time window
        while let Some(timestamp) = failure_window.front() {
            if now.duration_since(*timestamp) > self.health.tx_failure_time_window {
                failure_window.pop_front();
            } else {
                break;
            }
        }

        // Add the new failure timestamp
        failure_window.push_back(now);

        // If we've accumulated too many failures in the time window, mark the provider as failed
        if failure_window.len() >= self.health.tx_failure_threshold {
            let failure_msg = format!(
                "Provider '{}' marked unhealthy due to {} transaction failures within {}: {}",
                self.name,
                failure_window.len(),
                humantime::format_duration(self.health.tx_failure_time_window),
                reason
            );
            self.note_permanent_failure(failure_msg);
        } else {
            warn!(
                "Transaction failure detected on provider '{}': {}. ({} of {} failures within {})",
                self.name,
                reason,
                failure_window.len(),
                self.health.tx_failure_threshold,
                humantime::format_duration(self.health.tx_failure_time_window)
            );
        }
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
        provider_configs: NonEmpty<Endpoint>,
        initializer: I,
        transient_error_threshold: usize,
        tx_failure_threshold: usize,
        tx_failure_time_window: Duration,
    ) -> EthResult<Self> {
        let mut configs = VecDeque::from_iter(provider_configs);

        // Attempt to connect right away
        let provider_handle = connect_to_first_available_provider(
            &initializer,
            &mut configs,
            transient_error_threshold,
            tx_failure_threshold,
            tx_failure_time_window,
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
            tx_failure_threshold,
            tx_failure_time_window,
            permanent_cache: PermanentCache {
                commit_interval,
                contract_caller_address,
                blob_poster_address,
            },
            metrics: Default::default(),
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
            self.tx_failure_threshold,
            self.tx_failure_time_window,
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
                    // Increment network errors metric for fatal errors
                    self.metrics.eth_network_errors.inc();
                    Err(error)
                }
                ErrorClassification::Transient => {
                    provider.note_transient_error(&error);
                    // Increment network errors metric for transient errors
                    self.metrics.eth_network_errors.inc();
                    Err(error)
                }
                ErrorClassification::Other => Err(error),
            },
        }
    }

    /// Note a transaction failure for the current provider
    pub async fn note_tx_failure(&self, reason: &str) -> EthResult<()> {
        // Get the current provider without checking health since we want to mark it
        let provider = self.shared_state.lock().await.active_provider.clone();
        provider.note_tx_failure(reason).await;

        // Increment the tx failures counter
        self.metrics.eth_tx_failures.inc();

        Ok(())
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

    async fn note_tx_failure(&self, reason: &str) -> EthResult<()> {
        self.note_tx_failure(reason).await
    }
}

/// Attempts to connect to the first available provider, removing each config from
/// the front of the deque if it fails.
async fn connect_to_first_available_provider<I: ProviderInit>(
    initializer: &I,
    configs: &mut VecDeque<Endpoint>,
    transient_error_threshold: usize,
    tx_failure_threshold: usize,
    tx_failure_time_window: Duration,
) -> EthResult<ProviderHandle<I::Provider>> {
    while let Some(config) = configs.pop_front() {
        match initializer.initialize(&config).await {
            Ok(provider) => {
                let tracked = ProviderHandle::new(
                    config.name,
                    provider,
                    transient_error_threshold,
                    tx_failure_threshold,
                    tx_failure_time_window,
                );
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
    pub(crate) eth_tx_failures: IntCounter,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.eth_network_errors.clone()),
            Box::new(self.eth_tx_failures.clone()),
        ]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let eth_network_errors = IntCounter::with_opts(Opts::new(
            "eth_network_errors",
            "Number of network errors encountered while running Ethereum RPCs.",
        ))
        .expect("eth_network_errors metric to be correctly configured");

        let eth_tx_failures = IntCounter::with_opts(Opts::new(
            "eth_tx_failures",
            "Number of transaction failures potentially caused by provider issues.",
        ))
        .expect("eth_tx_failures metric to be correctly configured");

        Self {
            eth_network_errors,
            eth_tx_failures,
        }
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

impl<I> RegistersMetrics for FailoverClient<I>
where
    I: ProviderInit,
{
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.metrics.eth_network_errors.clone()),
            Box::new(self.metrics.eth_tx_failures.clone()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use services::types::nonempty;

    use super::*;
    use crate::provider::MockL1Provider; // Use the existing mock

    use std::sync::atomic::{AtomicUsize, Ordering};

    // A simple provider initializer that returns pre-configured providers
    struct TestProviderInitializer {
        providers: Vec<Arc<MockL1Provider>>,
        current_index: AtomicUsize,
    }

    impl TestProviderInitializer {
        fn new(providers: Vec<Arc<MockL1Provider>>) -> Self {
            Self {
                providers,
                current_index: AtomicUsize::new(0),
            }
        }
    }

    impl Clone for TestProviderInitializer {
        fn clone(&self) -> Self {
            Self {
                providers: self.providers.clone(),
                current_index: AtomicUsize::new(self.current_index.load(Ordering::Relaxed)),
            }
        }
    }

    impl ProviderInit for TestProviderInitializer {
        type Provider = MockL1Provider;

        async fn initialize(&self, config: &Endpoint) -> EthResult<Arc<Self::Provider>> {
            let index = self.current_index.fetch_add(1, Ordering::SeqCst);

            if index >= self.providers.len() {
                return Err(EthError::Other(format!(
                    "Failed to initialize provider: {}",
                    config.name
                )));
            }

            Ok(self.providers[index].clone())
        }
    }

    // An initializer that always fails
    #[derive(Debug)]
    struct FailingProviderInitializer;

    impl Clone for FailingProviderInitializer {
        fn clone(&self) -> Self {
            Self
        }
    }

    impl ProviderInit for FailingProviderInitializer {
        type Provider = MockL1Provider;

        async fn initialize(&self, config: &Endpoint) -> EthResult<Arc<Self::Provider>> {
            Err(EthError::Other(format!(
                "Failed to connect to {}",
                config.name
            )))
        }
    }

    // Convenience function to create a mock provider with defaults
    fn create_mock_provider() -> Arc<MockL1Provider> {
        let mut provider = MockL1Provider::new();

        // Set up default behavior for frequently called methods
        provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        provider
            .expect_blob_poster_address()
            .return_const(Some(Address::ZERO));

        Arc::new(provider)
    }

    // Helper to create provider that returns a permanent network error
    fn create_permanent_failure_provider() -> Arc<MockL1Provider> {
        let mut provider = MockL1Provider::new();
        provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        provider.expect_blob_poster_address().return_const(None);
        provider.expect_get_block_number().returning(|| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Permanent error".into(),
                    recoverable: false,
                })
            })
        });
        Arc::new(provider)
    }

    // Helper to create provider that returns a transient network error
    fn create_transient_failure_provider() -> Arc<MockL1Provider> {
        let mut provider = MockL1Provider::new();
        provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        provider.expect_blob_poster_address().return_const(None);
        provider.expect_get_block_number().returning(|| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Transient error".into(),
                    recoverable: true,
                })
            })
        });
        Arc::new(provider)
    }

    // Helper to create provider that succeeds with a specific block number
    fn create_successful_provider(block_number: u64) -> Arc<MockL1Provider> {
        let mut provider = MockL1Provider::new();
        provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        provider.expect_blob_poster_address().return_const(None);
        provider
            .expect_get_block_number()
            .returning(move || Box::pin(async move { Ok(block_number) }));
        Arc::new(provider)
    }

    #[tokio::test]
    async fn recoverable_error_does_not_immediately_make_unhealthy() {
        // Given: a provider that returns a recoverable network error
        // and a failover client with a threshold of 2 (so one error doesn't cross the limit)
        let mut provider = MockL1Provider::new();
        provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        provider.expect_blob_poster_address().return_const(None);
        provider.expect_submit().times(1).returning(|_, _| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Recoverable error".into(),
                    recoverable: true,
                })
            })
        });

        let initializer = TestProviderInitializer::new(vec![Arc::new(provider)]);
        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            2, // transient_error_threshold
            5,
            Duration::from_secs(300),
        )
        .await
        .unwrap();

        // When: a single submit call is made that returns a recoverable error
        let _ = client.submit([0; 32], 0).await;

        // Then: the client should still be healthy
        assert!(client.healthy().await);
    }

    #[tokio::test]
    async fn exceeding_recoverable_error_threshold_triggers_failover() {
        // Given: a provider that always returns a recoverable network error
        // and a failover client with a threshold of 1
        let mut first_provider = MockL1Provider::new();
        first_provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        first_provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        first_provider
            .expect_blob_poster_address()
            .return_const(None);
        // First provider can handle up to 1 calls before being marked unhealthy
        first_provider.expect_submit().times(1).returning(|_, _| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Recoverable error".into(),
                    recoverable: true,
                })
            })
        });

        // Second provider that returns success
        let mut second_provider = MockL1Provider::new();
        second_provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        second_provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        second_provider
            .expect_blob_poster_address()
            .return_const(None);
        // Second provider needs to handle all remaining calls
        second_provider
            .expect_submit()
            .return_once(|_, _| Box::pin(async { Ok(BlockSubmissionTx::default()) }));

        let initializer =
            TestProviderInitializer::new(vec![Arc::new(first_provider), Arc::new(second_provider)]);

        let client = FailoverClient::connect(
            nonempty![
                Endpoint {
                    name: "test1".to_owned(),
                    url: "http://example1.com".parse().unwrap(),
                },
                Endpoint {
                    name: "test2".to_owned(),
                    url: "http://example2.com".parse().unwrap(),
                },
            ],
            initializer,
            1, // transient_error_threshold
            5,
            Duration::from_secs(300),
        )
        .await
        .unwrap();

        // When: we exceed the transient error threshold
        let _ = client.submit([0; 32], 0).await; // First error
        let result = client.submit([0; 32], 0).await; // This should use the second provider

        // Then: the failover should happen successfully and the call should succeed
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn successful_call_resets_transient_error_count() {
        // Given: A provider that first errors then succeeds
        let mut provider = MockL1Provider::new();
        provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        provider.expect_blob_poster_address().return_const(None);
        provider.expect_submit().times(1).returning(|_, _| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Recoverable error".into(),
                    recoverable: true,
                })
            })
        });
        provider
            .expect_get_block_number()
            .times(1)
            .returning(|| Box::pin(async { Ok(10u64) }));

        let initializer = TestProviderInitializer::new(vec![Arc::new(provider)]);
        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            2, // transient_error_threshold
            5,
            Duration::from_secs(300),
        )
        .await
        .unwrap();

        // When: a submit call fails but then get_block_number succeeds
        let _ = client.submit([0; 32], 0).await; // This fails
        let result = client.get_block_number().await; // This succeeds

        // Then: the call should succeed and reset the error count
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 10u64);
        assert!(client.healthy().await);
    }

    #[tokio::test]
    async fn permanent_error_makes_connection_unhealthy() {
        // Given: a provider that returns a permanent network error
        let mut provider = MockL1Provider::new();
        provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        provider.expect_blob_poster_address().return_const(None);
        provider.expect_submit().times(1).returning(|_, _| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Permanent error".into(),
                    recoverable: false,
                })
            })
        });

        let initializer = TestProviderInitializer::new(vec![Arc::new(provider)]);
        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            3, // transient_error_threshold
            5,
            Duration::from_secs(300),
        )
        .await
        .unwrap();

        // When: a single submit call is made that returns a permanent error
        let _ = client.submit([0; 32], 0).await;

        // Then: the connection should be immediately unhealthy
        assert!(!client.healthy().await);
    }

    #[tokio::test]
    async fn other_error_does_not_affect_health() {
        // Given: a provider that returns a non-network error
        let mut provider = MockL1Provider::new();
        provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        provider.expect_blob_poster_address().return_const(None);
        provider.expect_submit().times(1).returning(|_, _| {
            Box::pin(async { Err(EthError::Other("Some application error".into())) })
        });

        let initializer = TestProviderInitializer::new(vec![Arc::new(provider)]);
        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            3, // transient_error_threshold
            5,
            Duration::from_secs(300),
        )
        .await
        .unwrap();

        // When: a submit call fails with a non-network error
        let result = client.submit([0; 32], 0).await;

        // Then: the connection health remains good despite the error
        assert!(result.is_err());
        assert!(client.healthy().await);
    }

    #[tokio::test]
    async fn multiple_provider_failover_chain() {
        // Given: Three providers where the first two fail and the third succeeds
        let providers = vec![
            create_permanent_failure_provider(), // First provider - permanent failure
            create_transient_failure_provider(), // Second provider - transient errors
            create_successful_provider(42u64),   // Third provider - succeeds
        ];

        let initializer = TestProviderInitializer::new(providers);

        let client = FailoverClient::connect(
            nonempty![
                Endpoint {
                    name: "test1".to_owned(),
                    url: "http://example1.com".parse().unwrap(),
                },
                Endpoint {
                    name: "test2".to_owned(),
                    url: "http://example2.com".parse().unwrap(),
                },
                Endpoint {
                    name: "test3".to_owned(),
                    url: "http://example3.com".parse().unwrap(),
                },
            ],
            initializer,
            1, // transient_error_threshold
            5,
            Duration::from_secs(300),
        )
        .await
        .unwrap();

        // When: we make calls that fail over through the providers
        let _ = client.get_block_number().await; // First provider fails permanently
        let _ = client.get_block_number().await; // Second provider first transient error
        let _ = client.get_block_number().await; // Second provider second transient error, fails over
        let result = client.get_block_number().await; // Third provider succeeds

        // Then: we should eventually get a successful result from the third provider
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42u64);
    }

    #[tokio::test]
    async fn tx_failure_tracking_marks_provider_unhealthy() {
        // Given: a provider and a client with a low tx failure threshold and short window
        let provider = create_mock_provider();
        let initializer = TestProviderInitializer::new(vec![provider]);

        let tx_failure_threshold = 2;
        let tx_failure_time_window = Duration::from_millis(500);

        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            3,
            tx_failure_threshold,
            tx_failure_time_window,
        )
        .await
        .unwrap();

        // When: we note multiple transaction failures within the time window
        client.note_tx_failure("First tx failure").await.unwrap();
        client.note_tx_failure("Second tx failure").await.unwrap();

        // Then: the provider should be marked unhealthy
        assert!(!client.healthy().await);
    }

    #[tokio::test]
    async fn tx_failures_outside_time_window_dont_affect_health() {
        // Given: a provider and a client with a low tx failure threshold and short window
        let provider = create_mock_provider();
        let initializer = TestProviderInitializer::new(vec![provider]);

        let tx_failure_threshold = 2;
        let tx_failure_time_window = Duration::from_millis(100);

        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            3,
            tx_failure_threshold,
            tx_failure_time_window,
        )
        .await
        .unwrap();

        // When: we note a tx failure, wait longer than the time window, then note another
        client.note_tx_failure("First tx failure").await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await; // Wait longer than the time window
        client.note_tx_failure("Second tx failure").await.unwrap();

        // Then: the provider should still be healthy because the failures are outside the time window
        assert!(client.healthy().await);
    }

    #[tokio::test]
    async fn initializer_failures_are_handled_gracefully() {
        // Given: an initializer that fails for all providers
        let initializer = FailingProviderInitializer;

        // When: we try to connect
        let result = FailoverClient::connect(
            nonempty![
                Endpoint {
                    name: "test1".to_owned(),
                    url: "http://example1.com".parse().unwrap(),
                },
                Endpoint {
                    name: "test2".to_owned(),
                    url: "http://example2.com".parse().unwrap(),
                },
            ],
            initializer,
            3,
            5,
            Duration::from_secs(300),
        )
        .await;

        // Then: the connection should fail with a meaningful error
        assert!(result.is_err());
        assert!(
            matches!(result.err().unwrap(), EthError::Other(msg) if msg == "no more providers available")
        );
    }

    #[tokio::test]
    async fn manually_marking_provider_permanently_failed() {
        // Given: a healthy provider and client
        let mut provider = MockL1Provider::new();
        provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        provider.expect_blob_poster_address().return_const(None);

        let initializer = TestProviderInitializer::new(vec![Arc::new(provider)]);
        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            3,
            5,
            Duration::from_secs(300),
        )
        .await
        .unwrap();

        // When: we manually mark the provider as permanently failed
        client
            .make_current_connection_permanently_failed("Manual failure".to_string())
            .await
            .unwrap();

        // Then: the connection should be unhealthy
        assert!(!client.healthy().await);
    }

    #[tokio::test]
    async fn metrics_are_incremented_properly() {
        // Given: a provider that returns network errors
        let mut provider = MockL1Provider::new();
        provider
            .expect_commit_interval()
            .return_const(NonZeroU32::new(10).unwrap());
        provider
            .expect_contract_caller_address()
            .return_const(Address::ZERO);
        provider.expect_blob_poster_address().return_const(None);
        provider.expect_submit().times(1).returning(|_, _| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Recoverable error".into(),
                    recoverable: true,
                })
            })
        });
        provider
            .expect_note_tx_failure()
            .returning(|_| Box::pin(async { Ok(()) }));

        let initializer = TestProviderInitializer::new(vec![Arc::new(provider)]);
        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            3,
            5,
            Duration::from_secs(300),
        )
        .await
        .unwrap();

        // Get metrics before
        let network_errors_before = client.metrics.eth_network_errors.get();
        let tx_failures_before = client.metrics.eth_tx_failures.get();

        // When: we make a call that returns a network error and note a tx failure
        let _ = client.submit([0; 32], 0).await;
        client.note_tx_failure("Test failure").await.unwrap();

        // Then: the metrics should be incremented
        assert!(client.metrics.eth_network_errors.get() > network_errors_before);
        assert!(client.metrics.eth_tx_failures.get() > tx_failures_before);
    }
}
