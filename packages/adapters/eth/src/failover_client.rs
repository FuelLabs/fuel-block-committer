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
use tracing::{debug, error, info, warn};

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

/// Configuration for health tracking
#[derive(Debug, Clone, Copy)]
pub struct HealthConfig {
    /// Maximum number of transient errors before considering a provider unhealthy
    pub transient_error_threshold: usize,
    /// Number of failed transactions to trigger unhealthy state
    pub tx_failure_threshold: usize,
    /// Time window to consider for transaction failures
    pub tx_failure_time_window: Duration,
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
    // Health configuration
    health_config: HealthConfig,
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
    health_config: HealthConfig,
    transient_error_count: AtomicUsize,
    permanently_failed: AtomicBool,
    // Track transactions that failed not due to network errors but due to mempool issues
    tx_failure_window: Mutex<VecDeque<Instant>>,
}

impl<P> ProviderHandle<P> {
    fn new(name: String, provider: Arc<P>, health_config: HealthConfig) -> Self {
        let health = Health {
            health_config,
            transient_error_count: AtomicUsize::new(0),
            permanently_failed: AtomicBool::new(false),
            tx_failure_window: Mutex::new(VecDeque::with_capacity(
                health_config.tx_failure_threshold + 1,
            )),
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
            "Transient connection error detected on provider '{}': {reason} \
             (Count: {current_count}/{})",
            self.name, self.health.health_config.transient_error_threshold
        );
    }

    pub fn is_healthy(&self) -> bool {
        // If the provider is marked permanently failed, it is not healthy.
        if self.health.permanently_failed.load(Ordering::Relaxed) {
            return false;
        }

        // If the transient error count is below threshold, it's still considered healthy.
        self.health.transient_error_count.load(Ordering::Relaxed)
            < self.health.health_config.transient_error_threshold
    }

    pub fn note_permanent_failure(&self, reason: impl Display) {
        error!("Provider '{}' permanently failed: {reason}", self.name);
        self.health
            .permanently_failed
            .store(true, Ordering::Relaxed);
    }

    pub async fn note_tx_failure(&self, reason: impl Display) {
        let now = Instant::now();
        let mut failure_window = self.health.tx_failure_window.lock().await;

        // Remove old entries outside the time window
        while let Some(timestamp) = failure_window.front() {
            if now.duration_since(*timestamp) > self.health.health_config.tx_failure_time_window {
                failure_window.pop_front();
            } else {
                break;
            }
        }

        // Add the new failure timestamp
        failure_window.push_back(now);

        // If we've accumulated too many failures in the time window, mark the provider as failed
        if failure_window.len() >= self.health.health_config.tx_failure_threshold {
            let failure_msg = format!(
                "Provider '{}' marked unhealthy due to {} transaction failures within {}: {}",
                self.name,
                failure_window.len(),
                humantime::format_duration(self.health.health_config.tx_failure_time_window),
                reason
            );
            self.note_permanent_failure(failure_msg);
        } else {
            warn!(
                "Transaction failure detected on provider '{}': {}. ({} of {} failures within {})",
                self.name,
                reason,
                failure_window.len(),
                self.health.health_config.tx_failure_threshold,
                humantime::format_duration(self.health.health_config.tx_failure_time_window)
            );
        }
    }
}

impl<I> FailoverClient<I>
where
    I: ProviderInit,
{
    /// This attempts to connect to the first available provider
    pub async fn connect(
        provider_configs: NonEmpty<Endpoint>,
        initializer: I,
        health_config: HealthConfig,
    ) -> EthResult<Self> {
        let mut configs = VecDeque::from_iter(provider_configs);

        // Attempt to connect right away
        let provider_handle =
            connect_to_first_available_provider(&initializer, &mut configs, &health_config).await?;

        let commit_interval = provider_handle.provider.commit_interval();
        let contract_caller_address = provider_handle.provider.contract_caller_address();
        let blob_poster_address = provider_handle.provider.blob_poster_address();

        let shared_state = SharedState {
            configs,
            active_provider: provider_handle,
        };

        Ok(Self {
            initializer,
            shared_state: Arc::new(Mutex::new(shared_state)),
            health_config,
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

        if state.active_provider.is_healthy() {
            debug!(
                "Using current provider '{}' which is healthy",
                state.active_provider.name
            );
            return Ok(state.active_provider.clone());
        }

        info!(
            "Current provider '{}' is unhealthy, attempting to fail over to next provider",
            state.active_provider.name
        );

        let handle = connect_to_first_available_provider(
            &self.initializer,
            &mut state.configs,
            &self.health_config,
        )
        .await?;

        info!("Successfully failed over to provider '{}'", handle.name);

        state.active_provider = handle.clone();
        Ok(handle)
    }

    /// Core abstraction to execute operations with failover logic
    async fn execute_operation<F, Fut, T>(&self, operation_factory: F) -> EthResult<T>
    where
        F: FnOnce(Arc<I::Provider>) -> Fut + Send + Sync,
        Fut: std::future::Future<Output = EthResult<T>> + Send,
        T: Send,
    {
        let provider = self.get_healthy_provider().await?;
        let provider_name = provider.name.clone();

        debug!("Executing operation on provider '{}'", provider_name);

        let result = operation_factory(Arc::clone(&provider.provider)).await;

        match result {
            Ok(value) => {
                debug!("Operation succeeded on provider '{}'", provider_name);
                provider.reset_transient_error_count();
                Ok(value)
            }
            Err(error) => {
                let error_type = match Self::classify_error(&error) {
                    ErrorClassification::Fatal => "fatal",
                    ErrorClassification::Transient => "transient",
                    ErrorClassification::Other => "other",
                };

                error!(
                    "Operation failed on provider '{}' with {} error: {}",
                    provider_name, error_type, error
                );

                match Self::classify_error(&error) {
                    ErrorClassification::Fatal => {
                        provider.note_permanent_failure(&error);
                        self.metrics.eth_network_errors.inc();
                        Err(error)
                    }
                    ErrorClassification::Transient => {
                        provider.note_transient_error(&error);
                        self.metrics.eth_network_errors.inc();
                        Err(error)
                    }
                    ErrorClassification::Other => Err(error),
                }
            }
        }
    }

    /// Note a transaction failure for the current provider
    pub async fn note_tx_failure(&self, reason: &str) -> EthResult<()> {
        // Get the current provider without checking health since we want to mark it
        let provider = self.shared_state.lock().await.active_provider.clone();
        let provider_name = provider.name.clone();

        info!(
            "Noting transaction failure on provider '{}': {}",
            provider_name, reason
        );

        provider.note_tx_failure(reason).await;

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
            provider.fees(height_range, reward_percentiles).await
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
                .submit_state_fragments(fragments, previous_tx, priority)
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
    health_config: &HealthConfig,
) -> EthResult<ProviderHandle<I::Provider>> {
    let mut attempts = 0;
    while let Some(config) = configs.pop_front() {
        attempts += 1;
        info!(
            "Attempting to connect to provider '{}' (attempt {})",
            config.name, attempts
        );

        match initializer.initialize(&config).await {
            Ok(provider) => {
                info!("Successfully connected to provider '{}'", config.name);

                let tracked = ProviderHandle::new(config.name, provider, *health_config);
                return Ok(tracked);
            }
            Err(err) => {
                warn!(
                    "Could not fail over to remote provider '{}': {err}",
                    config.name
                );
            }
        }
    }

    error!(
        "Failed to connect to any provider after {} attempts",
        attempts
    );
    Err(EthError::Other("no more providers available".into()))
}

use ::metrics::{
    RegistersMetrics,
    prometheus::{IntCounter, Opts, core::Collector},
};

#[derive(Clone)]
pub struct Metrics {
    eth_network_errors: IntCounter,
    eth_tx_failures: IntCounter,
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
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use services::types::nonempty;

    use super::*;
    use crate::provider::MockL1Provider;

    #[tokio::test]
    async fn recoverable_error_does_not_immediately_make_unhealthy() {
        // Given: a provider that returns a recoverable network error
        // and a failover client with a threshold of 2 (so one error doesn't cross the limit)
        let mut provider = create_mock_provider();
        provider.expect_submit().times(1).returning(|_, _| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Recoverable error".into(),
                    recoverable: true,
                })
            })
        });

        let initializer = TestProviderInitializer::new(vec![provider]);
        let health_config = HealthConfig {
            transient_error_threshold: 2,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };
        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            health_config,
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
        let mut first_provider = create_mock_provider();
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
        let mut second_provider = create_mock_provider();
        // Second provider needs to handle all remaining calls
        second_provider
            .expect_submit()
            .return_once(|_, _| Box::pin(async { Ok(BlockSubmissionTx::default()) }));

        let initializer = TestProviderInitializer::new([first_provider, second_provider]);
        let health_config = HealthConfig {
            transient_error_threshold: 1,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

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
            health_config,
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
        let mut provider = create_mock_provider();
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

        let initializer = TestProviderInitializer::new([provider]);
        let health_config = HealthConfig {
            transient_error_threshold: 2,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };
        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            health_config,
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
        let mut provider = create_mock_provider();
        provider.expect_submit().times(1).returning(|_, _| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Permanent error".into(),
                    recoverable: false,
                })
            })
        });

        let initializer = TestProviderInitializer::new([provider]);
        let health_config = HealthConfig {
            transient_error_threshold: 3,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };
        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            health_config,
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
        let mut provider = create_mock_provider();
        provider.expect_submit().times(1).returning(|_, _| {
            Box::pin(async { Err(EthError::Other("Some application error".into())) })
        });

        let initializer = TestProviderInitializer::new([provider]);
        let health_config = HealthConfig {
            transient_error_threshold: 3,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };
        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            health_config,
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
        let health_config = HealthConfig {
            transient_error_threshold: 1,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

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
            health_config,
        )
        .await
        .unwrap();

        // When: we make calls that fail over through the providers
        assert!(client.get_block_number().await.is_err()); // First provider fails permanently
        assert!(client.get_block_number().await.is_err()); // Second provider first transient error, fails over
        let result = client.get_block_number().await; // Third provider succeeds

        // Then: we should eventually get a successful result from the third provider
        assert_eq!(result.unwrap(), 42u64);
    }

    #[tokio::test]
    async fn tx_failure_tracking_marks_provider_unhealthy() {
        // Given: a provider and a client with a low tx failure threshold and short window
        let provider = create_mock_provider();
        let initializer = TestProviderInitializer::new([provider]);

        let tx_failure_threshold = 2;
        let tx_failure_time_window = Duration::from_millis(500);
        let health_config = HealthConfig {
            transient_error_threshold: 3,
            tx_failure_threshold,
            tx_failure_time_window,
        };

        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            health_config,
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
        let initializer = TestProviderInitializer::new([provider]);

        let tx_failure_threshold = 2;
        let tx_failure_time_window = Duration::from_millis(100);
        let health_config = HealthConfig {
            transient_error_threshold: 3,
            tx_failure_threshold,
            tx_failure_time_window,
        };

        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            health_config,
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
        let health_config = HealthConfig {
            transient_error_threshold: 3,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

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
            health_config,
        )
        .await;

        // Then: the connection should fail with a meaningful error
        assert!(result.is_err());
        assert!(
            matches!(result.err().unwrap(), EthError::Other(msg) if msg == "no more providers available")
        );
    }

    #[tokio::test]
    async fn metrics_are_incremented_properly() {
        // Given: a provider that returns network errors
        let mut provider = create_mock_provider();
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

        let initializer = TestProviderInitializer::new([provider]);
        let health_config = HealthConfig {
            transient_error_threshold: 3,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };
        let client = FailoverClient::connect(
            nonempty![Endpoint {
                name: "test".to_owned(),
                url: "http://example.com".parse().unwrap(),
            }],
            initializer,
            health_config,
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

    #[tokio::test]
    async fn all_providers_permanently_failed() {
        // Given: multiple providers that all return permanent network errors
        let providers = vec![
            create_permanent_failure_provider(), // First provider - permanent failure
            create_permanent_failure_provider(), // Second provider - permanent failure
            create_permanent_failure_provider(), // Third provider - permanent failure
        ];

        let initializer = TestProviderInitializer::new(providers);
        let health_config = HealthConfig {
            transient_error_threshold: 1,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

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
            health_config,
        )
        .await
        .unwrap();

        // When: we make calls that should fail over through each provider
        // First call - fails with permanent error from first provider
        let result1 = client.get_block_number().await;
        assert!(result1.is_err());
        assert!(matches!(
            result1.err().unwrap(),
            EthError::Network {
                recoverable: false,
                ..
            }
        ));

        // Second call - fails with permanent error from second provider
        let result2 = client.get_block_number().await;
        assert!(result2.is_err());
        assert!(matches!(
            result2.err().unwrap(),
            EthError::Network {
                recoverable: false,
                ..
            }
        ));

        // Third call - fails with permanent error from third provider
        let result3 = client.get_block_number().await;
        assert!(result3.is_err());
        assert!(matches!(
            result3.err().unwrap(),
            EthError::Network {
                recoverable: false,
                ..
            }
        ));

        // Fourth call - should fail because there are no more providers available
        let result4 = client.get_block_number().await;
        assert!(result4.is_err());
        assert!(matches!(
            result4.err().unwrap(),
            EthError::Other(msg) if msg == "no more providers available"
        ));

        // Then: the client should be unhealthy
        assert!(!client.healthy().await);
    }

    // A simple provider initializer that returns pre-configured mocks
    struct TestProviderInitializer {
        providers: Vec<Arc<MockL1Provider>>,
        current_index: AtomicUsize,
    }

    impl TestProviderInitializer {
        fn new(providers: impl IntoIterator<Item = MockL1Provider>) -> Self {
            let providers = providers.into_iter().map(Arc::new).collect();

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
    fn create_mock_provider() -> MockL1Provider {
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

        provider
    }

    // Helper to create provider that returns a permanent network error
    fn create_permanent_failure_provider() -> MockL1Provider {
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
        provider
    }

    // Helper to create provider that returns a transient network error
    fn create_transient_failure_provider() -> MockL1Provider {
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
        provider
    }

    // Helper to create provider that succeeds with a specific block number
    fn create_successful_provider(block_number: u64) -> MockL1Provider {
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
        provider
    }
}
