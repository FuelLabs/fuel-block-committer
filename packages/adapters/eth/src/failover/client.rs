use std::{collections::VecDeque, num::NonZeroU32, ops::RangeInclusive, sync::Arc};

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
use tracing::{debug, error, info};

use super::{
    error_tracker::ProviderHealthThresholds, metrics::Metrics, provider_handle::ProviderHandle,
};
use crate::{
    error::{Error as EthError, Result as EthResult},
    failover::error_tracker::{self, ErrorClassification},
    provider::L1Provider,
};

struct SharedState<P> {
    remaining_endpoints: VecDeque<Endpoint>,
    active_provider: ProviderHandle<P>,
}

#[derive(Clone)]
struct ProviderConstants {
    commit_interval: NonZeroU32,
    contract_caller_address: Address,
    blob_poster_address: Option<Address>,
}

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

/// A client that manages multiple L1 providers and fails over between them
/// when connection issues are detected.
#[derive(Clone)]
pub struct FailoverClient<I>
where
    I: ProviderInit,
{
    // Some object that can create providers of type I::Provider
    initializer: I,
    shared_state: Arc<Mutex<SharedState<I::Provider>>>,
    health_thresholds: ProviderHealthThresholds,
    /// Exists so that primitive getters don't have to be async and fallable just because they have to
    /// go through the failover logic.
    provider_constants: ProviderConstants,
    metrics: Metrics,
}

impl<I> FailoverClient<I>
where
    I: ProviderInit,
{
    pub async fn connect(
        initializer: I,
        health_thresholds: ProviderHealthThresholds,
        endpoints: NonEmpty<Endpoint>,
    ) -> EthResult<Self> {
        let mut remaining_endpoints = VecDeque::from_iter(endpoints);

        let provider_handle =
            connect_to_first_available_provider(&initializer, &mut remaining_endpoints).await?;

        let commit_interval = provider_handle.provider.commit_interval();
        let contract_caller_address = provider_handle.provider.contract_caller_address();
        let blob_poster_address = provider_handle.provider.blob_poster_address();

        let shared_state = SharedState {
            remaining_endpoints,
            active_provider: provider_handle,
        };

        Ok(Self {
            initializer,
            shared_state: Arc::new(Mutex::new(shared_state)),
            health_thresholds,
            provider_constants: ProviderConstants {
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

    /// Retrieves the currently active provider if it's healthy, or tries to fail over to the next.
    async fn get_healthy_provider(&self) -> EthResult<ProviderHandle<I::Provider>> {
        let mut state = self.shared_state.lock().await;

        if state
            .active_provider
            .is_healthy(&self.health_thresholds)
            .await
        {
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

        let handle =
            connect_to_first_available_provider(&self.initializer, &mut state.remaining_endpoints)
                .await?;

        info!("Successfully failed over to provider '{}'", handle.name);

        state.active_provider = handle.clone();
        Ok(handle)
    }

    /// Note a transaction failure for the current provider
    pub async fn note_tx_failure(&self, reason: &str) -> EthResult<()> {
        // Get the current provider without checking health since we don't want a failover here
        let provider = self.shared_state.lock().await.active_provider.clone();

        info!(
            "Noting transaction failure on provider '{}': {reason}",
            provider.name
        );

        provider
            .note_tx_failure(reason, self.health_thresholds.tx_failure_time_window)
            .await;

        self.metrics
            .eth_tx_failures
            .with_label_values(&[&provider.name])
            .inc();

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
        let provider_name = provider.name.as_str();

        debug!("Executing operation on provider '{}'", provider_name);

        let result = operation_factory(Arc::clone(&provider.provider)).await;

        match result {
            Ok(value) => {
                debug!("Operation succeeded on provider '{}'", provider_name);
                provider.reset_transient_error_count();
                Ok(value)
            }
            Err(error) => {
                let error_classification = error_tracker::classify_error(&error);

                error!(
                    "Operation failed on provider '{provider_name}' classified as {error_classification:?} error: {error}",
                );

                match error_classification {
                    ErrorClassification::Fatal => {
                        provider.note_permanent_failure(&error);
                        self.metrics
                            .eth_network_errors
                            .with_label_values(std::slice::from_ref(&provider_name))
                            .inc();
                        Err(error)
                    }
                    ErrorClassification::Transient => {
                        provider.note_transient_error(
                            &error,
                            self.health_thresholds.transient_error_threshold,
                        );

                        self.metrics
                            .eth_network_errors
                            .with_label_values(std::slice::from_ref(&provider_name))
                            .inc();
                        Err(error)
                    }
                    ErrorClassification::Other => Err(error),
                }
            }
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
        self.provider_constants.commit_interval
    }

    fn blob_poster_address(&self) -> Option<Address> {
        self.provider_constants.blob_poster_address
    }

    fn contract_caller_address(&self) -> Address {
        self.provider_constants.contract_caller_address
    }
}

/// Attempts to connect to the first available provider, removing each endpoint from
/// the front of the deque if it fails.
async fn connect_to_first_available_provider<I: ProviderInit>(
    initializer: &I,
    endpoints: &mut VecDeque<Endpoint>,
) -> EthResult<ProviderHandle<I::Provider>> {
    let mut attempts = 0;
    while let Some(config) = endpoints.pop_front() {
        attempts += 1;
        info!(
            "Attempting to connect to provider '{}' (attempt {})",
            config.name, attempts
        );

        match initializer.initialize(&config).await {
            Ok(provider) => {
                info!("Successfully connected to provider '{}'", config.name);

                return Ok(ProviderHandle::new(config.name, provider));
            }
            Err(err) => {
                error!(
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

#[async_trait::async_trait]
impl<I> HealthCheck for FailoverClient<I>
where
    I: ProviderInit,
{
    async fn healthy(&self) -> bool {
        self.get_healthy_provider().await.is_ok()
    }
}

impl<I> ::metrics::RegistersMetrics for FailoverClient<I>
where
    I: ProviderInit,
{
    fn metrics(&self) -> Vec<Box<dyn ::metrics::prometheus::core::Collector>> {
        self.metrics.metrics()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use services::types::nonempty;

    use super::*;
    use crate::provider::MockL1Provider;

    #[tokio::test]
    async fn recoverable_error_does_not_immediately_make_unhealthy() {
        // Given: a provider that returns a recoverable network error
        // and a failover client with a threshold of 2 (so one error doesn't cross the limit)
        let mut provider = create_mock_provider();
        provider.expect_submit().return_once(|_, _| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Recoverable error".into(),
                    recoverable: true,
                })
            })
        });

        let initializer = MockProviderFactory::new(vec![provider]);
        let health_thresholds = ProviderHealthThresholds {
            transient_error_threshold: 2,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

        let endpoints = nonempty![Endpoint {
            name: "test".to_owned(),
            url: "http://example.com".parse().unwrap(),
        }];

        let client = FailoverClient::connect(initializer, health_thresholds, endpoints)
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
        first_provider.expect_submit().return_once(|_, _| {
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

        let initializer = MockProviderFactory::new([first_provider, second_provider]);
        let health_thresholds = ProviderHealthThresholds {
            transient_error_threshold: 1,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

        let endpoints = nonempty![
            Endpoint {
                name: "test1".to_owned(),
                url: "http://example1.com".parse().unwrap(),
            },
            Endpoint {
                name: "test2".to_owned(),
                url: "http://example2.com".parse().unwrap(),
            },
        ];

        let client = FailoverClient::connect(initializer, health_thresholds, endpoints)
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
        // Given: A provider that first errors, then succeeds, then errors again
        let mut provider = create_mock_provider();
        
        // First call - error
        provider.expect_submit().return_once(|_, _| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Recoverable error 1".into(),
                    recoverable: true,
                })
            })
        });
        
        // Second call - success (should reset error count)
        provider
            .expect_get_block_number()
            .return_once(|| Box::pin(async { Ok(10u64) }));
            
        // Third call - error again
        // If error count wasn't reset, this would be the second error and would trigger failover
        provider.expect_balance().return_once(|_| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Recoverable error 2".into(),
                    recoverable: true,
                })
            })
        });

        let initializer = MockProviderFactory::new([provider]);
        let health_thresholds = ProviderHealthThresholds {
            transient_error_threshold: 2, // Two errors required to trigger failover
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

        let endpoints = nonempty![Endpoint {
            name: "test".to_owned(),
            url: "http://example.com".parse().unwrap(),
        }];

        let client = FailoverClient::connect(initializer, health_thresholds, endpoints)
            .await
            .unwrap();

        // When: a submit call fails, then get_block_number succeeds, then balance fails
        let submit_result = client.submit([0; 32], 0).await; // First error
        assert!(submit_result.is_err());
        
        let block_result = client.get_block_number().await; // Success - should reset error count
        assert!(block_result.is_ok());
        assert_eq!(block_result.unwrap(), 10u64);
        
        let balance_result = client.balance(Address::ZERO).await; // Second error
        assert!(balance_result.is_err());

        // Then: the connection should still be healthy because the successful call reset the error count
        // If the counter wasn't reset, we'd have 2 errors and the provider would be unhealthy
        assert!(client.healthy().await);
    }

    #[tokio::test]
    async fn permanent_error_makes_connection_unhealthy() {
        // Given: a provider that returns a permanent network error
        let mut provider = create_mock_provider();
        provider.expect_submit().return_once(|_, _| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Permanent error".into(),
                    recoverable: false,
                })
            })
        });

        let initializer = MockProviderFactory::new([provider]);
        let health_thresholds = ProviderHealthThresholds {
            transient_error_threshold: 3,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

        let endpoints = nonempty![Endpoint {
            name: "test".to_owned(),
            url: "http://example.com".parse().unwrap(),
        }];

        let client = FailoverClient::connect(initializer, health_thresholds, endpoints)
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
        provider.expect_submit().return_once(|_, _| {
            Box::pin(async { Err(EthError::Other("Some application error".into())) })
        });

        let initializer = MockProviderFactory::new([provider]);
        let health_thresholds = ProviderHealthThresholds {
            transient_error_threshold: 3,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

        let endpoints = nonempty![Endpoint {
            name: "test".to_owned(),
            url: "http://example.com".parse().unwrap(),
        }];

        let client = FailoverClient::connect(initializer, health_thresholds, endpoints)
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

        let initializer = MockProviderFactory::new(providers);
        let health_thresholds = ProviderHealthThresholds {
            transient_error_threshold: 1,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

        let endpoints = nonempty![
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
        ];

        let client = FailoverClient::connect(initializer, health_thresholds, endpoints)
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
        let initializer = MockProviderFactory::new([provider]);

        let tx_failure_threshold = 2;
        let tx_failure_time_window = Duration::from_millis(500);
        let health_thresholds = ProviderHealthThresholds {
            transient_error_threshold: 3,
            tx_failure_threshold,
            tx_failure_time_window,
        };

        let endpoints = nonempty![Endpoint {
            name: "test".to_owned(),
            url: "http://example.com".parse().unwrap(),
        }];

        let client = FailoverClient::connect(initializer, health_thresholds, endpoints)
            .await
            .unwrap();

        // When: we note multiple transaction failures within the time window
        client.note_tx_failure("First tx failure").await.unwrap();
        client.note_tx_failure("Second tx failure").await.unwrap();

        // This will cause client.healthy() to return false since it uses get_healthy_provider
        // which will attempt to fail over because the current provider is unhealthy
        assert!(!client.healthy().await);
    }

    #[tokio::test]
    async fn tx_failures_outside_time_window_dont_affect_health() {
        // Given: a provider and a client with a low tx failure threshold and short window
        let provider = create_mock_provider();
        let initializer = MockProviderFactory::new([provider]);

        let tx_failure_threshold = 2;
        let tx_failure_time_window = Duration::from_millis(100);
        let health_thresholds = ProviderHealthThresholds {
            transient_error_threshold: 3,
            tx_failure_threshold,
            tx_failure_time_window,
        };

        let endpoints = nonempty![Endpoint {
            name: "test".to_owned(),
            url: "http://example.com".parse().unwrap(),
        }];

        let client = FailoverClient::connect(initializer, health_thresholds, endpoints)
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
        let health_thresholds = ProviderHealthThresholds {
            transient_error_threshold: 3,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

        let endpoints = nonempty![
            Endpoint {
                name: "test1".to_owned(),
                url: "http://example1.com".parse().unwrap(),
            },
            Endpoint {
                name: "test2".to_owned(),
                url: "http://example2.com".parse().unwrap(),
            },
        ];

        // When: we try to connect
        let result = FailoverClient::connect(initializer, health_thresholds, endpoints).await;

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
        provider.expect_submit().return_once(|_, _| {
            Box::pin(async {
                Err(EthError::Network {
                    msg: "Recoverable error".into(),
                    recoverable: true,
                })
            })
        });

        let initializer = MockProviderFactory::new([provider]);
        let health_thresholds = ProviderHealthThresholds {
            transient_error_threshold: 3,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

        let provider_name = "test";
        let endpoints = nonempty![Endpoint {
            name: provider_name.to_owned(),
            url: "http://example.com".parse().unwrap(),
        }];

        let client = FailoverClient::connect(initializer, health_thresholds, endpoints)
            .await
            .unwrap();

        // Get metrics before
        let network_errors_before = client
            .metrics
            .eth_network_errors
            .with_label_values(std::slice::from_ref(&provider_name))
            .get();
        let tx_failures_before = client
            .metrics
            .eth_tx_failures
            .with_label_values(std::slice::from_ref(&provider_name))
            .get();

        // When: we make a call that returns a network error and note a tx failure
        let _ = client.submit([0; 32], 0).await;
        client.note_tx_failure("Test failure").await.unwrap();

        // Then: the metrics should be incremented
        assert!(
            client
                .metrics
                .eth_network_errors
                .with_label_values(std::slice::from_ref(&provider_name))
                .get()
                > network_errors_before
        );
        assert!(
            client
                .metrics
                .eth_tx_failures
                .with_label_values(std::slice::from_ref(&provider_name))
                .get()
                > tx_failures_before
        );
    }

    #[tokio::test]
    async fn all_providers_permanently_failed() {
        // Given: multiple providers that all return permanent network errors
        let providers = vec![
            create_permanent_failure_provider(), // First provider - permanent failure
            create_permanent_failure_provider(), // Second provider - permanent failure
            create_permanent_failure_provider(), // Third provider - permanent failure
        ];

        let initializer = MockProviderFactory::new(providers);
        let health_thresholds = ProviderHealthThresholds {
            transient_error_threshold: 1,
            tx_failure_threshold: 5,
            tx_failure_time_window: Duration::from_secs(300),
        };

        let endpoints = nonempty![
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
        ];

        let client = FailoverClient::connect(initializer, health_thresholds, endpoints)
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
    struct MockProviderFactory {
        providers: Vec<Arc<MockL1Provider>>,
        current_index: AtomicUsize,
    }

    impl MockProviderFactory {
        fn new(providers: impl IntoIterator<Item = MockL1Provider>) -> Self {
            let providers = providers.into_iter().map(Arc::new).collect();

            Self {
                providers,
                current_index: AtomicUsize::new(0),
            }
        }
    }

    impl Clone for MockProviderFactory {
        fn clone(&self) -> Self {
            Self {
                providers: self.providers.clone(),
                current_index: AtomicUsize::new(self.current_index.load(Ordering::Relaxed)),
            }
        }
    }

    impl ProviderInit for MockProviderFactory {
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
