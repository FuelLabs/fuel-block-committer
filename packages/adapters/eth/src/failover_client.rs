use std::{
    collections::VecDeque,
    fmt::Display,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, MutexGuard, RwLock};
use tracing::{debug, error, info, warn};

use crate::error::{Error as EthError, Result as EthResult};
use crate::provider::{L1Provider, ProviderConfig, ProviderFactory};
use alloy::primitives::Address;
use alloy::rpc::types::FeeHistory;
use services::state_committer::port::l1::Priority;
use services::types::{
    BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx, NonEmpty, TransactionResponse, U256,
};
use std::ops::RangeInclusive;

// Define the threshold for transient errors
const TRANSIENT_ERROR_THRESHOLD: usize = 3;

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
/// when connection issues are detected
pub struct FailoverClient<P> {
    /// List of provider configurations
    configs: Arc<Mutex<VecDeque<ProviderConfig>>>,
    /// Factory to create new provider instances
    provider_factory: ProviderFactory<P>,
    /// The currently active provider
    active_provider: Arc<Mutex<Tracked<Arc<P>>>>,
}

struct Tracked<P> {
    name: String,
    provider: P,
    max_transient_errors: usize,
    transient_error_count: AtomicUsize,
    permanently_failed: AtomicBool,
}

impl<P> Tracked<P> {
    fn new(name: String, provider: P, max_transient_errors: usize) -> Self {
        Self {
            name,
            provider,
            max_transient_errors,
            transient_error_count: AtomicUsize::new(0),
            permanently_failed: AtomicBool::new(false),
        }
    }

    pub fn reset_transient_error_count(&self) {
        self.transient_error_count.store(0, Ordering::Relaxed);
    }

    pub fn note_transient_error(&self, reason: impl Display) {
        let current_count = self.transient_error_count.fetch_add(1, Ordering::Relaxed) + 1;

        warn!(
            "Transient connection error detected: {reason} (Count: {current_count}/{TRANSIENT_ERROR_THRESHOLD})",
        );
    }

    pub fn is_unhealthy(&self) -> bool {
        self.transient_error_count.load(Ordering::Relaxed) >= self.max_transient_errors
            || self.permanently_failed.load(Ordering::Relaxed)
    }

    pub fn note_permanent_failure(&self, reason: impl Display) {
        warn!("Provider '{}' permanently failed: {reason}", self.name);
        self.permanently_failed.store(true, Ordering::Relaxed);
    }
}

impl<P> FailoverClient<P> {
    /// Create a new FailoverClient with the given provider configurations and factory
    pub async fn connect(
        provider_configs: Vec<ProviderConfig>,
        provider_factory: ProviderFactory<P>,
    ) -> EthResult<Self> {
        let mut configs = VecDeque::from(provider_configs.clone());
        let provider = connect_to_first_available_provider(&provider_factory, &mut configs).await?;

        Ok(Self {
            configs: Arc::new(Mutex::new(configs)),
            provider_factory,
            active_provider: Arc::new(Mutex::new(provider)),
        })
    }

    /// Helper to classify errors based on their types
    fn classify_error(err: &EthError) -> ErrorClassification {
        match err {
            // Fatal errors that should immediately trigger failover
            EthError::Network {
                recoverable: false, ..
            } => ErrorClassification::Fatal,

            // Transient errors that should only trigger failover after threshold
            EthError::Network {
                recoverable: true, ..
            } => ErrorClassification::Transient,

            // Request-specific errors that shouldn't trigger failover
            EthError::TxExecution(_) | EthError::Other(_) => ErrorClassification::Other,
        }
    }

    /// Try to connect to the next available provider that hasn't failed yet
    async fn get_provider(&self) -> EthResult<MutexGuard<Tracked<Arc<P>>>> {
        let mut current_provider_lock = self.active_provider.lock().await;

        if !current_provider_lock.is_unhealthy() {
            return Ok(current_provider_lock);
        }

        let mut configs = self.configs.lock().await;
        let provider =
            connect_to_first_available_provider(&self.provider_factory, &mut configs).await?;
        *current_provider_lock = provider;

        Err(EthError::Other("no more providers available".into()))
    }

    pub async fn make_current_connection_permanently_failed(
        &self,
        reason: String,
    ) -> EthResult<()> {
        self.active_provider
            .lock()
            .await
            .note_permanent_failure(reason);
        Ok(())
    }

    /// Core abstraction to execute operations with failover logic
    async fn execute_operation<F, Fut, T>(&self, operation_factory: F) -> EthResult<T>
    where
        F: Fn(Arc<P>) -> Fut + Send + Sync,
        Fut: Future<Output = EthResult<T>> + Send,
        T: Send,
    {
        let provider = self.get_provider().await?;

        let result = operation_factory(Arc::clone(&provider.provider)).await;

        match result {
            Ok(value) => {
                provider.reset_transient_error_count();
                Ok(value)
            }
            Err(error) => {
                match Self::classify_error(&error) {
                    ErrorClassification::Fatal => {
                        provider.note_permanent_failure(&error);

                        Err(error)
                    }
                    ErrorClassification::Transient => {
                        provider.note_transient_error(&error);

                        Err(error)
                    }
                    ErrorClassification::Other => {
                        // Safer to not use other errors to reset the transient error count
                        Err(error)
                    }
                }
            }
        }
    }

    // Public methods that implement the L1Provider trait methods using execute_operation

    pub async fn get_block_number(&self) -> EthResult<u64>
    where
        P: L1Provider,
    {
        self.execute_operation(|provider| async move { provider.get_block_number().await })
            .await
    }

    pub async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> EthResult<Option<TransactionResponse>>
    where
        P: L1Provider,
    {
        self.execute_operation(move |provider| {
            let tx_hash_clone = tx_hash;
            async move { provider.get_transaction_response(tx_hash_clone).await }
        })
        .await
    }

    pub async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> EthResult<bool>
    where
        P: L1Provider,
    {
        self.execute_operation(move |provider| {
            let tx_hash_clone = tx_hash;
            async move { provider.is_squeezed_out(tx_hash_clone).await }
        })
        .await
    }

    pub async fn balance(&self, address: Address) -> EthResult<U256>
    where
        P: L1Provider,
    {
        self.execute_operation(move |provider| {
            let address_clone = address;
            async move { provider.balance(address_clone).await }
        })
        .await
    }

    pub async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
        reward_percentiles: &[f64],
    ) -> EthResult<FeeHistory>
    where
        P: L1Provider,
    {
        let reward_percentiles = reward_percentiles.to_vec();
        self.execute_operation(move |provider| {
            let range_clone = height_range.clone();
            let percentiles_clone = reward_percentiles.clone();

            async move { provider.fees(range_clone, &percentiles_clone).await }
        })
        .await
    }

    pub async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<services::types::L1Tx>,
        priority: Priority,
    ) -> EthResult<(L1Tx, FragmentsSubmitted)>
    where
        P: L1Provider,
    {
        self.execute_operation(move |provider| {
            let fragments_clone = fragments.clone();
            let previous_tx_clone = previous_tx.clone();
            let priority_clone = priority;

            async move {
                provider
                    .submit_state_fragments(fragments_clone, previous_tx_clone, priority_clone)
                    .await
            }
        })
        .await
    }

    pub async fn submit(&self, hash: [u8; 32], height: u32) -> EthResult<BlockSubmissionTx>
    where
        P: L1Provider,
    {
        self.execute_operation(move |provider| {
            let hash_clone = hash;
            let height_clone = height;

            async move { provider.submit(hash_clone, height_clone).await }
        })
        .await
    }
}

async fn connect_to_first_available_provider<P>(
    provider_factory: &ProviderFactory<P>,
    configs: &mut VecDeque<ProviderConfig>,
) -> EthResult<Tracked<Arc<P>>> {
    while let Some(config) = configs.pop_front() {
        match (provider_factory)(&config).await {
            Ok(provider) => {
                let tracked = Tracked::new(
                    config.name,
                    Arc::clone(&provider),
                    TRANSIENT_ERROR_THRESHOLD,
                );
                return Ok(tracked);
            }
            Err(err) => {
                warn!(
                    "could not failover to remote provider {}: {err}",
                    config.name
                );
            }
        }
    }

    Err(EthError::Other("no more providers available".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::MockL1Provider;
    use crate::provider::tests::create_mock_provider_factory;
    use mockall::predicate::*;

    // #[tokio::test]
    // async fn test_successful_call_first_provider() {
    //     // Given: A client with a single provider that returns success
    //     let config_urls = vec!["mock://p1"];
    //     let configs = vec![ProviderConfig::new("first", config_urls[0])];
    //
    //     // Create mock provider and set expectations
    //     let mut mock_provider = MockL1Provider::new();
    //     mock_provider
    //         .expect_get_block_number()
    //         .times(1)
    //         .returning(|| Ok(100));
    //
    //     // Pack providers into (url, provider) pairs
    //     let providers = vec![(config_urls[0].to_string(), mock_provider)];
    //
    //     let factory = create_mock_provider_factory(providers);
    //     let client = FailoverClient::connect(configs, factory).await?;
    //
    //     // When: Making a call to get_block_number
    //     let block_num = client.get_block_number().await.unwrap();
    //
    //     // Then: We should get the expected response
    //     assert_eq!(block_num, 100);
    //     assert_eq!(client.current_index.load(Ordering::Relaxed), 0);
    //     assert_eq!(client.transient_error_count.load(Ordering::Relaxed), 0);
    //     assert!(!client.permanently_failed.load(Ordering::Relaxed));
    // }

    // #[tokio::test]
    // async fn test_failover_on_fatal_error() {
    //     // Given: A client with two providers where the first one returns a fatal error and the second succeeds
    //     let config_urls = vec!["mock://p1", "mock://p2"];
    //     let configs = vec![
    //         ProviderConfig::new("first", config_urls[0]),
    //         ProviderConfig::new("second", config_urls[1]),
    //     ];
    //
    //     // Create first mock provider with fatal error
    //     let mut mock_provider1 = MockL1Provider::new();
    //     mock_provider1
    //         .expect_get_block_number()
    //         .times(1)
    //         .returning(|| {
    //             Err(EthError::Network {
    //                 msg: "Backend connection dropped".into(),
    //                 recoverable: false,
    //             })
    //         });
    //
    //     // Create second mock provider - we don't retry operations after failover
    //     let mut mock_provider2 = MockL1Provider::new();
    //     // But we'll need this provider for subsequent operations
    //     mock_provider2
    //         .expect_get_block_number()
    //         .times(1)
    //         .returning(|| Ok(200));
    //
    //     // Pack providers into (url, provider) pairs
    //     let providers = vec![
    //         (String::from("mock://p1"), mock_provider1),
    //         (String::from("mock://p2"), mock_provider2),
    //     ];
    //
    //     let factory = create_mock_provider_factory(providers);
    //     let client = FailoverClient::connect(configs, factory).await.unwrap();
    //
    //     // When: Making a call to get_block_number, it will fail with provider 1 and trigger failover
    //     let result = client.get_block_number().await;
    //
    //     // Then: The operation should fail but the client should have failed over to provider 2
    //     assert!(result.is_err());
    //
    //     // Check that provider 1 is marked as failed
    //     {
    //         let failed_providers = client.failed_providers.lock().await;
    //         assert_eq!(failed_providers.len(), 1);
    //         assert!(failed_providers.contains(&0));
    //     }
    //
    //     // Verify we've switched to provider 2
    //     assert_eq!(client.current_index.load(Ordering::Relaxed), 1);
    //     assert_eq!(client.transient_error_count.load(Ordering::Relaxed), 0);
    //     assert!(!client.permanently_failed.load(Ordering::Relaxed));
    //
    //     // A subsequent call should succeed using provider 2
    //     let second_result = client.get_block_number().await;
    //     assert!(second_result.is_ok());
    //     assert_eq!(second_result.unwrap(), 200);
    // }

    // #[tokio::test]
    // async fn test_failover_on_transient_error_threshold() {
    //     // Given: A client with two providers where the first one returns transient errors
    //     let config_urls = vec!["mock://p1", "mock://p2"];
    //     let configs = vec![
    //         ProviderConfig::new(config_urls[0]),
    //         ProviderConfig::new(config_urls[1]),
    //     ];
    //
    //     // Create first mock provider with transient errors
    //     let mut mock_provider1 = MockL1Provider::new();
    //     mock_provider1
    //         .expect_get_block_number()
    //         .times(TRANSIENT_ERROR_THRESHOLD) // We need exactly THRESHOLD errors to trigger failover
    //         .returning(|| {
    //             Err(EthError::Network {
    //                 msg: "Timeout".into(),
    //                 recoverable: true,
    //             })
    //         });
    //
    //     // Create second mock provider for later calls
    //     let mut mock_provider2 = MockL1Provider::new();
    //     mock_provider2
    //         .expect_get_block_number()
    //         .times(1)
    //         .returning(|| Ok(200));
    //
    //     // Pack providers into (url, provider) pairs
    //     let providers = vec![
    //         (String::from("mock://p1"), mock_provider1),
    //         (String::from("mock://p2"), mock_provider2),
    //     ];
    //
    //     let factory = create_mock_provider_factory(providers);
    //     let client = FailoverClient::new(configs, factory);
    //
    //     // Make calls until we reach the threshold
    //     for i in 1..=TRANSIENT_ERROR_THRESHOLD {
    //         let result = client.get_block_number().await;
    //         assert!(result.is_err());
    //         // On the last error, failover should be triggered
    //     }
    //
    //     // After the last error, failover should have happened
    //     // Check that provider 1 is marked as failed
    //     {
    //         let failed_providers = client.failed_providers.lock().await;
    //         assert_eq!(failed_providers.len(), 1);
    //         assert!(failed_providers.contains(&0));
    //     }
    //
    //     // Verify we've switched to provider 2
    //     assert_eq!(client.current_index.load(Ordering::Relaxed), 1);
    //
    //     // A subsequent call should succeed using provider 2
    //     let final_result = client.get_block_number().await;
    //     assert!(final_result.is_ok());
    //     assert_eq!(final_result.unwrap(), 200);
    // }
    //
    // #[tokio::test]
    // async fn test_external_fault_report_triggers_failover() {
    //     // Given: A client with two providers, both working
    //     let config_urls = vec!["mock://p1", "mock://p2"];
    //     let configs = vec![
    //         ProviderConfig::new(config_urls[0]),
    //         ProviderConfig::new(config_urls[1]),
    //     ];
    //
    //     // Create first mock provider
    //     let mut mock_provider1 = MockL1Provider::new();
    //     mock_provider1
    //         .expect_get_block_number()
    //         .times(1)
    //         .returning(|| Ok(100));
    //
    //     // Create second mock provider
    //     let mut mock_provider2 = MockL1Provider::new();
    //     mock_provider2
    //         .expect_get_block_number()
    //         .times(1)
    //         .returning(|| Ok(300));
    //
    //     // Pack providers into (url, provider) pairs
    //     let providers = vec![
    //         (String::from("mock://p1"), mock_provider1),
    //         (String::from("mock://p2"), mock_provider2),
    //     ];
    //
    //     let factory = create_mock_provider_factory(providers);
    //     let client = FailoverClient::new(configs, factory);
    //
    //     // Initial call should use the first provider
    //     let initial_result = client.get_block_number().await.unwrap();
    //     assert_eq!(initial_result, 100);
    //     assert_eq!(client.current_index.load(Ordering::Relaxed), 0);
    //
    //     // When: An external system reports an issue
    //     client
    //         .make_current_connection_permanently_failed(
    //             "Transactions disappeared from mempool".into(),
    //         )
    //         .await;
    //
    //     // Then: We should have marked provider 1 as failed
    //     {
    //         let failed_providers = client.failed_providers.lock().await;
    //         assert_eq!(failed_providers.len(), 1);
    //         assert!(failed_providers.contains(&0));
    //     }
    //
    //     // And switched to provider 2
    //     assert_eq!(client.current_index.load(Ordering::Relaxed), 1);
    //
    //     // Next call should use the second provider
    //     let result = client.get_block_number().await.unwrap();
    //     assert_eq!(result, 300);
    //
    //     // If we try to report a fault on provider 2, we should be marked as permanently failed
    //     // since we've exhausted all available providers
    //     client
    //         .make_current_connection_permanently_failed("Another issue".into())
    //         .await;
    //
    //     // Verify both providers are marked as failed and client is permanently failed
    //     {
    //         let failed_providers = client.failed_providers.lock().await;
    //         assert_eq!(failed_providers.len(), 2);
    //         assert!(failed_providers.contains(&0));
    //         assert!(failed_providers.contains(&1));
    //     }
    //
    //     assert!(client.permanently_failed.load(Ordering::Relaxed));
    // }
    //
    // #[tokio::test]
    // async fn test_all_providers_fail() {
    //     // Given: A client with two providers
    //     let config_urls = vec!["mock://p1", "mock://p2"];
    //     let configs = vec![
    //         ProviderConfig::new(config_urls[0]),
    //         ProviderConfig::new(config_urls[1]),
    //     ];
    //
    //     // Create first mock provider that will fail
    //     let mut mock_provider1 = MockL1Provider::new();
    //     mock_provider1
    //         .expect_get_block_number()
    //         .times(1)
    //         .returning(|| {
    //             Err(EthError::Network {
    //                 msg: "Backend connection dropped".into(),
    //                 recoverable: false,
    //             })
    //         });
    //
    //     // Create second mock provider that will also fail when used in a later call
    //     let mut mock_provider2 = MockL1Provider::new();
    //     mock_provider2
    //         .expect_get_block_number()
    //         .times(1)
    //         .returning(|| {
    //             Err(EthError::Network {
    //                 msg: "Backend connection dropped".into(),
    //                 recoverable: false,
    //             })
    //         });
    //
    //     // Pack providers into (url, provider) pairs
    //     let providers = vec![
    //         (String::from("mock://p1"), mock_provider1),
    //         (String::from("mock://p2"), mock_provider2),
    //     ];
    //
    //     let factory = create_mock_provider_factory(providers);
    //     let client = FailoverClient::new(configs, factory);
    //
    //     // First call - provider 1 fails
    //     let result1 = client.get_block_number().await;
    //     assert!(result1.is_err());
    //
    //     // Provider 1 should be marked as failed and we should be using provider 2
    //     {
    //         let failed_providers = client.failed_providers.lock().await;
    //         assert_eq!(failed_providers.len(), 1);
    //         assert!(failed_providers.contains(&0));
    //     }
    //     assert_eq!(client.current_index.load(Ordering::Relaxed), 1);
    //     assert!(!client.permanently_failed.load(Ordering::Relaxed));
    //
    //     // Second call - provider 2 fails
    //     let result2 = client.get_block_number().await;
    //     assert!(result2.is_err());
    //
    //     // Both providers should now be marked as failed
    //     {
    //         let failed_providers = client.failed_providers.lock().await;
    //         assert_eq!(failed_providers.len(), 2);
    //         assert!(failed_providers.contains(&0));
    //         assert!(failed_providers.contains(&1));
    //     }
    //     assert!(client.permanently_failed.load(Ordering::Relaxed));
    //
    //     // Third call - should immediately fail without trying any provider
    //     let result3 = client.get_block_number().await;
    //     assert!(result3.is_err());
    //     // Error message should indicate that all providers are permanently failed
    //     assert!(matches!(result3, Err(EthError::Other(msg)) if msg.contains("permanently failed")));
    // }
    //
    // #[tokio::test]
    // async fn test_request_errors_dont_trigger_failover() {
    //     // Given: A client with a provider that returns a request error (non-connection)
    //     let config_urls = vec!["mock://p1"];
    //     let configs = vec![ProviderConfig::new(config_urls[0])];
    //
    //     // Create mock provider with request error then success
    //     let mut mock_provider = MockL1Provider::new();
    //     mock_provider
    //         .expect_get_block_number()
    //         .times(1)
    //         .returning(|| Err(EthError::TxExecution("Insufficient funds".into())));
    //
    //     mock_provider
    //         .expect_get_block_number()
    //         .times(1)
    //         .returning(|| Ok(100));
    //
    //     // Pack provider into (url, provider) pair
    //     let providers = vec![(config_urls[0].to_string(), mock_provider)];
    //
    //     let factory = create_mock_provider_factory(providers);
    //     let client = FailoverClient::new(configs, factory);
    //
    //     // When: Making a call that results in a request error
    //     let result = client.get_block_number().await;
    //
    //     // Then: The call should fail but not trigger failover
    //     assert!(result.is_err());
    //     assert_eq!(client.current_index.load(Ordering::Relaxed), 0);
    //     assert_eq!(client.transient_error_count.load(Ordering::Relaxed), 0);
    //     assert!(!client.permanently_failed.load(Ordering::Relaxed));
    //
    //     // And the next call should still use the same provider
    //     let second_result = client.get_block_number().await.unwrap();
    //     assert_eq!(second_result, 100);
    // }
}
