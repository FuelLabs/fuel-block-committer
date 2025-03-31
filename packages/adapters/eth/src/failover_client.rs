use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::error::{Error as EthError, Result as EthResult};
use crate::provider::{L1Provider, ProviderConfig, ProviderFactory};
use alloy::primitives::{Address, Bytes, TxHash};
use alloy::rpc::types::FeeHistory;
use async_trait::async_trait;
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
    Request,
}

/// A client that manages multiple L1 providers and fails over between them
/// when connection issues are detected
pub struct FailoverClient {
    /// List of provider configurations
    configs: Vec<ProviderConfig>,
    /// Factory to create new provider instances
    provider_factory: ProviderFactory,
    /// Index of the currently active provider in the configs list
    current_index: AtomicUsize,
    /// The currently active provider
    active_provider: Mutex<Option<Arc<dyn L1Provider>>>,
    /// Whether all providers have been tried and failed
    permanently_failed: AtomicBool,
    /// Counter for tracking consecutive transient errors
    transient_error_count: AtomicUsize,
    /// Set of indices of providers that have been tried and failed
    /// Once a provider fails, we never use it again for the lifetime of the application
    failed_providers: Mutex<Vec<usize>>,
}

impl FailoverClient {
    /// Create a new FailoverClient with the given provider configurations and factory
    pub fn new(provider_configs: Vec<ProviderConfig>, provider_factory: ProviderFactory) -> Self {
        Self {
            configs: provider_configs,
            provider_factory,
            current_index: AtomicUsize::new(0),
            active_provider: Mutex::new(None),
            permanently_failed: AtomicBool::new(false),
            transient_error_count: AtomicUsize::new(0),
            failed_providers: Mutex::new(Vec::new()),
        }
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
            EthError::TxExecution(_) | EthError::Other(_) => ErrorClassification::Request,
        }
    }

    /// Try to connect to the next available provider that hasn't failed yet
    async fn try_connect_next(&self) -> EthResult<()> {
        if self.permanently_failed.load(Ordering::Relaxed) {
            return Err(EthError::Other("All providers permanently failed".into()));
        }

        let mut active_provider_guard = self.active_provider.lock().await;
        let current_provider_index = self.current_index.load(Ordering::Relaxed);
        
        // Mark the current provider as failed, but only if we actually have an active provider
        // (this prevents marking a provider as failed during initial connection)
        if active_provider_guard.is_some() {
            let mut failed_providers = self.failed_providers.lock().await;
            if !failed_providers.contains(&current_provider_index) {
                failed_providers.push(current_provider_index);
            }
        }
        
        // Drop the previous provider
        *active_provider_guard = None;

        // Find the next available provider that hasn't failed yet
        let mut failed_providers = self.failed_providers.lock().await;
        let available_indices: Vec<usize> = (0..self.configs.len())
            .filter(|idx| !failed_providers.contains(idx))
            .collect();

        if available_indices.is_empty() {
            // We've tried all providers and none worked
            error!("All providers have been tried and failed");
            self.permanently_failed.store(true, Ordering::Relaxed);
            return Err(EthError::Other("All providers permanently failed".into()));
        }

        // Log the available providers for debugging
        info!(
            "Available providers after failover: {:?}",
            available_indices
        );

        // Try each available provider in order
        for &attempt_index in &available_indices {
            let config = &self.configs[attempt_index];
            info!(
                "Attempting connection to provider at {}",
                config.display_name()
            );

            // Try to create a provider using the factory
            match (self.provider_factory)(config).await {
                Ok(provider_instance) => {
                    info!(
                        "Successfully connected to provider at {}",
                        config.display_name()
                    );
                    *active_provider_guard = Some(provider_instance);
                    self.current_index.store(attempt_index, Ordering::Relaxed);
                    self.transient_error_count.store(0, Ordering::Relaxed);
                    return Ok(());
                }
                Err(e) => {
                    warn!(
                        "Failed to connect to provider at {}: {:?}",
                        config.display_name(),
                        e
                    );

                    // Mark this provider as failed
                    if !failed_providers.contains(&attempt_index) {
                        failed_providers.push(attempt_index);
                    }
                }
            }
        }

        // If we get here, all providers have failed
        error!("All available providers have failed");
        self.permanently_failed.store(true, Ordering::Relaxed);
        Err(EthError::Other("All providers permanently failed".into()))
    }

    /// Report an issue with the current provider from an external system
    pub async fn report_current_connection_issue(&self, reason: String) {
        // Avoid triggering failover if already permanently failed
        if self.permanently_failed.load(Ordering::Relaxed) {
            warn!(
                "Ignoring external fault report ({}): already permanently failed",
                reason
            );
            return;
        }

        let current_idx = self.current_index.load(Ordering::Relaxed);
        let provider_name = self
            .configs
            .get(current_idx)
            .map(|c| c.display_name())
            .unwrap_or("unknown");

        info!(
            "External fault reported for provider {}: {}. Initiating failover.",
            provider_name, reason
        );

        // Mark the current provider as failed
        {
            let mut failed_providers = self.failed_providers.lock().await;
            if !failed_providers.contains(&current_idx) {
                failed_providers.push(current_idx);
            }
        }

        // Directly attempt to connect to the next provider
        match self.try_connect_next().await {
            Ok(_) => {
                let new_idx = self.current_index.load(Ordering::Relaxed);
                let new_provider_name = self
                    .configs
                    .get(new_idx)
                    .map(|c| c.display_name())
                    .unwrap_or("unknown");

                info!(
                    "Failover successful after external fault report. New active provider: {}",
                    new_provider_name
                );
            }
            Err(e) => {
                // This likely means all providers failed
                error!(
                    "Failover failed after external fault report: {:?}. Service may be permanently degraded.",
                    e
                );
            }
        }
    }

    /// Core abstraction to execute operations with failover logic
    async fn execute_operation<F, Fut, T>(&self, operation_factory: F) -> EthResult<T>
    where
        F: Fn(Arc<dyn L1Provider>) -> Fut + Send + Sync,
        Fut: Future<Output = EthResult<T>> + Send,
        T: Send,
    {
        // Check for permanent failure state - if all providers are unhealthy, fail fast
        if self.permanently_failed.load(Ordering::Relaxed) {
            return Err(EthError::Other("All providers permanently failed".into()));
        }

        // Get or establish the active provider
        let provider_instance = {
            let mut guard = self.active_provider.lock().await;
            if guard.is_none() {
                // Lazy connection attempt if no provider is active
                debug!("No active provider. Attempting initial connection/failover...");
                // Unlock the guard *before* calling try_connect_next to avoid deadlock potential
                drop(guard);
                match self.try_connect_next().await {
                    Ok(_) => (),
                    Err(e) => return Err(e), // Propagate error (likely all providers failed)
                }

                // Re-acquire the lock to get the newly connected provider
                guard = self.active_provider.lock().await;
            }

            // Clone the Arc to release the lock sooner
            guard
                .as_ref()
                .ok_or_else(|| {
                    error!("Error: Active provider is None despite not being permanently failed");
                    EthError::Other("No active provider".into())
                })?
                .clone()
        };

        // Execute the actual operation by calling the factory closure
        let result = operation_factory(provider_instance).await;

        match result {
            Ok(value) => {
                // Reset transient error count on successful operation
                let old_count = self.transient_error_count.swap(0, Ordering::Relaxed);
                if old_count > 0 {
                    debug!(
                        "Operation successful, resetting transient error count from {}",
                        old_count
                    );
                }
                Ok(value)
            }
            Err(error) => {
                match Self::classify_error(&error) {
                    ErrorClassification::Fatal => {
                        warn!(
                            "Fatal connection error detected: {:?}. Marking provider as unhealthy and triggering failover.",
                            error
                        );
                        // Reset any transient error count when handling a fatal error
                        self.transient_error_count.store(0, Ordering::Relaxed);
                        
                        // Try to connect to next provider, but don't retry the current operation
                        // Just mark the current one as failed and return the original error
                        if let Err(e) = self.try_connect_next().await {
                            // If we couldn't fail over, it means all providers are now unhealthy
                            error!("Failover failed after fatal error: {:?}", e);
                            return Err(e);
                        }
                        
                        // Successfully failed over to a new provider, but still return the original error
                        // The next call will use the new provider
                        Err(error)
                    }
                    ErrorClassification::Transient => {
                        let current_count =
                            self.transient_error_count.fetch_add(1, Ordering::Relaxed) + 1;
                        warn!(
                            "Transient connection error detected: {:?} (Count: {}/{})",
                            error, current_count, TRANSIENT_ERROR_THRESHOLD
                        );

                        if current_count >= TRANSIENT_ERROR_THRESHOLD {
                            warn!("Transient error threshold exceeded. Marking provider as unhealthy and triggering failover.");
                            // Reset the counter immediately when we trigger failover
                            self.transient_error_count.store(0, Ordering::Relaxed);
                            
                            // Try to connect to next provider, but don't retry the current operation
                            if let Err(e) = self.try_connect_next().await {
                                // If we couldn't fail over, it means all providers are now unhealthy
                                error!("Failover failed after transient threshold: {:?}", e);
                                return Err(e);
                            }
                        }
                        
                        // Return original error regardless - either we failed over but still return the error,
                        // or we're below the threshold so just return the error
                        Err(error)
                    }
                    ErrorClassification::Request => {
                        // Non-connection error, reset transient count for this connection
                        let old_count = self.transient_error_count.swap(0, Ordering::Relaxed);
                        if old_count > 0 {
                            debug!(
                                "Request error encountered: {:?}. Resetting transient error count from {}",
                                error, old_count
                            );
                        }
                        Err(error)
                    }
                }
            }
        }
    }

    // Public methods that implement the L1Provider trait methods using execute_operation

    pub async fn get_block_number(&self) -> EthResult<u64> {
        self.execute_operation(|provider| async move { provider.get_block_number().await })
            .await
    }

    pub async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> EthResult<Option<TransactionResponse>> {
        self.execute_operation(move |provider| {
            let tx_hash_clone = tx_hash;
            async move { provider.get_transaction_response(tx_hash_clone).await }
        })
        .await
    }

    pub async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> EthResult<bool> {
        self.execute_operation(move |provider| {
            let tx_hash_clone = tx_hash;
            async move { provider.is_squeezed_out(tx_hash_clone).await }
        })
        .await
    }

    pub async fn balance(&self, address: Address) -> EthResult<U256> {
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
    ) -> EthResult<FeeHistory> {
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
    ) -> EthResult<(L1Tx, FragmentsSubmitted)> {
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

    pub async fn submit(&self, hash: [u8; 32], height: u32) -> EthResult<BlockSubmissionTx> {
        self.execute_operation(move |provider| {
            let hash_clone = hash;
            let height_clone = height;

            async move { provider.submit(hash_clone, height_clone).await }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::MockL1Provider;
    use crate::provider::tests::create_mock_provider_factory;
    use mockall::predicate::*;

    #[tokio::test]
    async fn test_successful_call_first_provider() {
        // Given: A client with a single provider that returns success
        let config_urls = vec!["mock://p1"];
        let configs = vec![ProviderConfig::new(config_urls[0])];

        // Create mock provider and set expectations
        let mut mock_provider = MockL1Provider::new();
        mock_provider
            .expect_get_block_number()
            .times(1)
            .returning(|| Ok(100));

        // Pack providers into (url, provider) pairs
        let providers = vec![(config_urls[0].to_string(), mock_provider)];

        let factory = create_mock_provider_factory(providers);
        let client = FailoverClient::new(configs, factory);

        // When: Making a call to get_block_number
        let block_num = client.get_block_number().await.unwrap();

        // Then: We should get the expected response
        assert_eq!(block_num, 100);
        assert_eq!(client.current_index.load(Ordering::Relaxed), 0);
        assert_eq!(client.transient_error_count.load(Ordering::Relaxed), 0);
        assert!(!client.permanently_failed.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_failover_on_fatal_error() {
        // Given: A client with two providers where the first one returns a fatal error and the second succeeds
        let config_urls = vec!["mock://p1", "mock://p2"];
        let configs = vec![
            ProviderConfig::new(config_urls[0]),
            ProviderConfig::new(config_urls[1]),
        ];

        // Create first mock provider with fatal error
        let mut mock_provider1 = MockL1Provider::new();
        mock_provider1
            .expect_get_block_number()
            .times(1)
            .returning(|| {
                Err(EthError::Network {
                    msg: "Backend connection dropped".into(),
                    recoverable: false,
                })
            });

        // Create second mock provider - we don't retry operations after failover
        let mut mock_provider2 = MockL1Provider::new();
        // But we'll need this provider for subsequent operations
        mock_provider2
            .expect_get_block_number()
            .times(1)
            .returning(|| Ok(200));

        // Pack providers into (url, provider) pairs
        let providers = vec![
            (String::from("mock://p1"), mock_provider1),
            (String::from("mock://p2"), mock_provider2),
        ];

        let factory = create_mock_provider_factory(providers);
        let client = FailoverClient::new(configs, factory);

        // When: Making a call to get_block_number, it will fail with provider 1 and trigger failover
        let result = client.get_block_number().await;

        // Then: The operation should fail but the client should have failed over to provider 2
        assert!(result.is_err());

        // Check that provider 1 is marked as failed
        {
            let failed_providers = client.failed_providers.lock().await;
            assert_eq!(failed_providers.len(), 1);
            assert!(failed_providers.contains(&0));
        }

        // Verify we've switched to provider 2
        assert_eq!(client.current_index.load(Ordering::Relaxed), 1);
        assert_eq!(client.transient_error_count.load(Ordering::Relaxed), 0);
        assert!(!client.permanently_failed.load(Ordering::Relaxed));

        // A subsequent call should succeed using provider 2
        let second_result = client.get_block_number().await;
        assert!(second_result.is_ok());
        assert_eq!(second_result.unwrap(), 200);
    }

    #[tokio::test]
    async fn test_failover_on_transient_error_threshold() {
        // Given: A client with two providers where the first one returns transient errors
        let config_urls = vec!["mock://p1", "mock://p2"];
        let configs = vec![
            ProviderConfig::new(config_urls[0]),
            ProviderConfig::new(config_urls[1]),
        ];
        
        // Create first mock provider with transient errors
        let mut mock_provider1 = MockL1Provider::new();
        mock_provider1
            .expect_get_block_number()
            .times(TRANSIENT_ERROR_THRESHOLD) // We need exactly THRESHOLD errors to trigger failover
            .returning(|| {
                Err(EthError::Network {
                    msg: "Timeout".into(),
                    recoverable: true,
                })
            });
        
        // Create second mock provider for later calls
        let mut mock_provider2 = MockL1Provider::new();
        mock_provider2
            .expect_get_block_number()
            .times(1)
            .returning(|| Ok(200));
        
        // Pack providers into (url, provider) pairs
        let providers = vec![
            (String::from("mock://p1"), mock_provider1),
            (String::from("mock://p2"), mock_provider2),
        ];
        
        let factory = create_mock_provider_factory(providers);
        let client = FailoverClient::new(configs, factory);

        // Make calls until we reach the threshold
        for i in 1..=TRANSIENT_ERROR_THRESHOLD {
            let result = client.get_block_number().await;
            assert!(result.is_err());
            // On the last error, failover should be triggered
        }
        
        // After the last error, failover should have happened
        // Check that provider 1 is marked as failed
        {
            let failed_providers = client.failed_providers.lock().await;
            assert_eq!(failed_providers.len(), 1);
            assert!(failed_providers.contains(&0));
        }
        
        // Verify we've switched to provider 2
        assert_eq!(client.current_index.load(Ordering::Relaxed), 1);
        
        // A subsequent call should succeed using provider 2
        let final_result = client.get_block_number().await;
        assert!(final_result.is_ok());
        assert_eq!(final_result.unwrap(), 200);
    }

    #[tokio::test]
    async fn test_external_fault_report_triggers_failover() {
        // Given: A client with two providers, both working
        let config_urls = vec!["mock://p1", "mock://p2"];
        let configs = vec![
            ProviderConfig::new(config_urls[0]),
            ProviderConfig::new(config_urls[1]),
        ];

        // Create first mock provider
        let mut mock_provider1 = MockL1Provider::new();
        mock_provider1
            .expect_get_block_number()
            .times(1)
            .returning(|| Ok(100));

        // Create second mock provider
        let mut mock_provider2 = MockL1Provider::new();
        mock_provider2
            .expect_get_block_number()
            .times(1)
            .returning(|| Ok(300));

        // Pack providers into (url, provider) pairs
        let providers = vec![
            (String::from("mock://p1"), mock_provider1),
            (String::from("mock://p2"), mock_provider2),
        ];

        let factory = create_mock_provider_factory(providers);
        let client = FailoverClient::new(configs, factory);

        // Initial call should use the first provider
        let initial_result = client.get_block_number().await.unwrap();
        assert_eq!(initial_result, 100);
        assert_eq!(client.current_index.load(Ordering::Relaxed), 0);

        // When: An external system reports an issue
        client
            .report_current_connection_issue("Transactions disappeared from mempool".into())
            .await;

        // Then: We should have marked provider 1 as failed
        {
            let failed_providers = client.failed_providers.lock().await;
            assert_eq!(failed_providers.len(), 1);
            assert!(failed_providers.contains(&0));
        }

        // And switched to provider 2
        assert_eq!(client.current_index.load(Ordering::Relaxed), 1);

        // Next call should use the second provider
        let result = client.get_block_number().await.unwrap();
        assert_eq!(result, 300);

        // If we try to report a fault on provider 2, we should be marked as permanently failed
        // since we've exhausted all available providers
        client
            .report_current_connection_issue("Another issue".into())
            .await;

        // Verify both providers are marked as failed and client is permanently failed
        {
            let failed_providers = client.failed_providers.lock().await;
            assert_eq!(failed_providers.len(), 2);
            assert!(failed_providers.contains(&0));
            assert!(failed_providers.contains(&1));
        }

        assert!(client.permanently_failed.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_all_providers_fail() {
        // Given: A client with two providers
        let config_urls = vec!["mock://p1", "mock://p2"];
        let configs = vec![
            ProviderConfig::new(config_urls[0]),
            ProviderConfig::new(config_urls[1]),
        ];
        
        // Create first mock provider that will fail
        let mut mock_provider1 = MockL1Provider::new();
        mock_provider1
            .expect_get_block_number()
            .times(1)
            .returning(|| {
                Err(EthError::Network {
                    msg: "Backend connection dropped".into(),
                    recoverable: false,
                })
            });
        
        // Create second mock provider that will also fail when used in a later call
        let mut mock_provider2 = MockL1Provider::new();
        mock_provider2
            .expect_get_block_number()
            .times(1)
            .returning(|| {
                Err(EthError::Network {
                    msg: "Backend connection dropped".into(),
                    recoverable: false,
                })
            });
        
        // Pack providers into (url, provider) pairs
        let providers = vec![
            (String::from("mock://p1"), mock_provider1),
            (String::from("mock://p2"), mock_provider2),
        ];
        
        let factory = create_mock_provider_factory(providers);
        let client = FailoverClient::new(configs, factory);

        // First call - provider 1 fails
        let result1 = client.get_block_number().await;
        assert!(result1.is_err());
        
        // Provider 1 should be marked as failed and we should be using provider 2
        {
            let failed_providers = client.failed_providers.lock().await;
            assert_eq!(failed_providers.len(), 1);
            assert!(failed_providers.contains(&0));
        }
        assert_eq!(client.current_index.load(Ordering::Relaxed), 1);
        assert!(!client.permanently_failed.load(Ordering::Relaxed));
        
        // Second call - provider 2 fails
        let result2 = client.get_block_number().await;
        assert!(result2.is_err());
        
        // Both providers should now be marked as failed
        {
            let failed_providers = client.failed_providers.lock().await;
            assert_eq!(failed_providers.len(), 2);
            assert!(failed_providers.contains(&0));
            assert!(failed_providers.contains(&1));
        }
        assert!(client.permanently_failed.load(Ordering::Relaxed));
        
        // Third call - should immediately fail without trying any provider
        let result3 = client.get_block_number().await;
        assert!(result3.is_err());
        // Error message should indicate that all providers are permanently failed
        assert!(matches!(result3, Err(EthError::Other(msg)) if msg.contains("permanently failed")));
    }

    #[tokio::test]
    async fn test_request_errors_dont_trigger_failover() {
        // Given: A client with a provider that returns a request error (non-connection)
        let config_urls = vec!["mock://p1"];
        let configs = vec![ProviderConfig::new(config_urls[0])];

        // Create mock provider with request error then success
        let mut mock_provider = MockL1Provider::new();
        mock_provider
            .expect_get_block_number()
            .times(1)
            .returning(|| Err(EthError::TxExecution("Insufficient funds".into())));

        mock_provider
            .expect_get_block_number()
            .times(1)
            .returning(|| Ok(100));

        // Pack provider into (url, provider) pair
        let providers = vec![(config_urls[0].to_string(), mock_provider)];

        let factory = create_mock_provider_factory(providers);
        let client = FailoverClient::new(configs, factory);

        // When: Making a call that results in a request error
        let result = client.get_block_number().await;

        // Then: The call should fail but not trigger failover
        assert!(result.is_err());
        assert_eq!(client.current_index.load(Ordering::Relaxed), 0);
        assert_eq!(client.transient_error_count.load(Ordering::Relaxed), 0);
        assert!(!client.permanently_failed.load(Ordering::Relaxed));

        // And the next call should still use the same provider
        let second_result = client.get_block_number().await.unwrap();
        assert_eq!(second_result, 100);
    }
}
