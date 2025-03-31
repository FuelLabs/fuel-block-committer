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

    /// Try to connect to the next available provider
    async fn try_connect_next(&self) -> EthResult<()> {
        if self.permanently_failed.load(Ordering::Relaxed) {
            return Err(EthError::Other("All providers permanently failed".into()));
        }

        let mut active_provider_guard = self.active_provider.lock().await;
        let current_provider_index = self.current_index.load(Ordering::Relaxed);
        *active_provider_guard = None; // Drop previous provider

        // Start with the next provider index
        let mut attempt_index = (current_provider_index + 1) % self.configs.len();
        let start_attempt_index = attempt_index;

        info!(
            "Attempting failover. Starting check from index {}",
            attempt_index
        );

        loop {
            // Check if we've been marked permanently failed (by another concurrent operation)
            if self.permanently_failed.load(Ordering::Relaxed) {
                return Err(EthError::Other("All providers permanently failed".into()));
            }

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

                    // Move to the next provider index
                    attempt_index = (attempt_index + 1) % self.configs.len();

                    // Check if we've tried all providers in this cycle
                    if attempt_index == start_attempt_index {
                        // We've cycled through all available providers and none worked
                        error!("Exhausted all providers during failover attempt");
                        self.permanently_failed.store(true, Ordering::Relaxed);
                        return Err(EthError::Other("All providers failed".into()));
                    }
                    // Continue loop to try the next provider
                }
            }
        }
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
        F: Fn(Arc<dyn L1Provider>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = EthResult<T>> + Send,
        T: Send,
    {
        loop {
            // Check for permanent failure state
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
                guard.clone().ok_or_else(|| {
                    error!("Error: Active provider is None despite not being permanently failed");
                    EthError::Other("No active provider".into())
                })?
            }; // Lock guard is dropped here

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
                    return Ok(value);
                }
                Err(error) => {
                    match Self::classify_error(&error) {
                        ErrorClassification::Fatal => {
                            warn!(
                                "Fatal connection error detected: {:?}. Triggering failover.",
                                error
                            );
                            match self.try_connect_next().await {
                                Ok(_) => {
                                    info!(
                                        "Failover successful after fatal error. Retrying operation."
                                    );
                                    continue; // Retry operation with new connection
                                }
                                Err(e) => {
                                    error!("Failover failed after fatal error.");
                                    return Err(e); // Propagate failover error
                                }
                            }
                        }
                        ErrorClassification::Transient => {
                            let current_count =
                                self.transient_error_count.fetch_add(1, Ordering::Relaxed) + 1;
                            warn!(
                                "Transient connection error detected: {:?} (Count: {}/{})",
                                error, current_count, TRANSIENT_ERROR_THRESHOLD
                            );

                            if current_count >= TRANSIENT_ERROR_THRESHOLD {
                                warn!("Transient error threshold exceeded. Triggering failover.");
                                match self.try_connect_next().await {
                                    Ok(_) => {
                                        info!(
                                            "Failover successful after transient threshold. Retrying operation."
                                        );
                                        continue; // Retry operation with new connection
                                    }
                                    Err(e) => {
                                        error!("Failover failed after transient threshold.");
                                        return Err(e); // Propagate failover error
                                    }
                                }
                            } else {
                                // Threshold not exceeded, return original error
                                return Err(error);
                            }
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
                            return Err(error);
                        }
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
    use crate::provider::tests::{MockL1Provider, MockResponseType, create_mock_provider_factory};
    use std::collections::HashMap;

    // Helper function to create a map of mock responses
    fn create_mock_responses(
        configs: &[ProviderConfig],
        responses: Vec<Vec<EthResult<MockResponseType>>>,
    ) -> Arc<Mutex<HashMap<String, Vec<EthResult<MockResponseType>>>>> {
        let mut map = HashMap::new();
        for (i, config) in configs.iter().enumerate() {
            map.insert(
                config.url.clone(),
                responses.get(i).cloned().unwrap_or_default(),
            );
        }
        Arc::new(Mutex::new(map))
    }

    #[tokio::test]
    async fn test_successful_call_first_provider() {
        // Given: A client with a single provider that returns success
        let configs = vec![ProviderConfig::new("mock://p1")];
        let responses = vec![vec![Ok(MockResponseType::BlockNumber(100))]];
        let mock_responses = create_mock_responses(&configs, responses);
        let factory = create_mock_provider_factory(mock_responses);

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
        let configs = vec![
            ProviderConfig::new("mock://p1"),
            ProviderConfig::new("mock://p2"),
        ];
        let responses = vec![
            vec![Err(EthError::Network {
                msg: "Backend connection dropped".into(),
                recoverable: false,
            })],
            vec![Ok(MockResponseType::BlockNumber(200))],
        ];
        let mock_responses = create_mock_responses(&configs, responses);
        let factory = create_mock_provider_factory(mock_responses);

        let client = FailoverClient::new(configs, factory);

        // When: Making a call to get_block_number
        let block_num = client.get_block_number().await.unwrap();

        // Then: We should get the response from the second provider after failover
        assert_eq!(block_num, 200);
        assert_eq!(client.current_index.load(Ordering::Relaxed), 1);
        assert_eq!(client.transient_error_count.load(Ordering::Relaxed), 0);
        assert!(!client.permanently_failed.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_failover_on_transient_error_threshold() {
        // Given: A client with two providers where the first one returns transient errors
        let configs = vec![
            ProviderConfig::new("mock://p1"),
            ProviderConfig::new("mock://p2"),
        ];

        // First provider has 4 responses: 3 transient errors (below threshold), then 1 more to trigger failover
        let p1_responses = vec![
            Err(EthError::Network {
                msg: "Timeout".into(),
                recoverable: true,
            }),
            Err(EthError::Network {
                msg: "Timeout".into(),
                recoverable: true,
            }),
            Err(EthError::Network {
                msg: "Timeout".into(),
                recoverable: true,
            }),
            Err(EthError::Network {
                msg: "Timeout".into(),
                recoverable: true,
            }),
        ];

        // Second provider succeeds
        let p2_responses = vec![Ok(MockResponseType::BlockNumber(200))];

        let mock_responses = create_mock_responses(&configs, vec![p1_responses, p2_responses]);
        let factory = create_mock_provider_factory(mock_responses);

        let client = FailoverClient::new(configs, factory);

        // When: Making multiple calls to get_block_number
        let res1 = client.get_block_number().await;
        assert!(res1.is_err());
        assert_eq!(client.transient_error_count.load(Ordering::Relaxed), 1);

        let res2 = client.get_block_number().await;
        assert!(res2.is_err());
        assert_eq!(client.transient_error_count.load(Ordering::Relaxed), 2);

        let res3 = client.get_block_number().await;
        assert!(res3.is_err());
        assert_eq!(client.transient_error_count.load(Ordering::Relaxed), 3);

        // This should trigger failover after exceeding threshold
        let res4 = client.get_block_number().await;

        // Then: The fourth call should succeed with the second provider
        assert_eq!(res4.unwrap(), 200);
        assert_eq!(client.current_index.load(Ordering::Relaxed), 1);
        assert_eq!(client.transient_error_count.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_external_fault_report_triggers_failover() {
        // Given: A client with two providers, both working
        let configs = vec![
            ProviderConfig::new("mock://p1"),
            ProviderConfig::new("mock://p2"),
        ];
        let responses = vec![
            vec![Ok(MockResponseType::BlockNumber(100))],
            vec![Ok(MockResponseType::BlockNumber(200))],
        ];
        let mock_responses = create_mock_responses(&configs, responses);
        let factory = create_mock_provider_factory(mock_responses);

        let mut client = FailoverClient::new(configs, factory);

        // Initial call should use the first provider
        let initial_result = client.get_block_number().await.unwrap();
        assert_eq!(initial_result, 100);
        assert_eq!(client.current_index.load(Ordering::Relaxed), 0);

        // When: An external system reports an issue
        client
            .report_current_connection_issue("Transactions disappeared from mempool".into())
            .await;

        // Then: The next call should use the second provider
        // Since we've already consumed the responses for the first provider, we'll need to prepare new ones
        let new_responses = vec![
            vec![],                                       // No more responses for p1
            vec![Ok(MockResponseType::BlockNumber(300))], // New response for p2
        ];
        let new_mock_responses = create_mock_responses(
            &[
                ProviderConfig::new("mock://p1"),
                ProviderConfig::new("mock://p2"),
            ],
            new_responses,
        );

        let new_factory = create_mock_provider_factory(new_mock_responses);
        client.provider_factory = new_factory;

        // Now check that we're using the second provider
        assert_eq!(client.current_index.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_all_providers_fail() {
        // Given: A client with two providers, both failing with fatal errors
        let configs = vec![
            ProviderConfig::new("mock://p1"),
            ProviderConfig::new("mock://p2"),
        ];
        let responses = vec![
            vec![Err(EthError::Network {
                msg: "Backend connection dropped".into(),
                recoverable: false,
            })],
            vec![Err(EthError::Network {
                msg: "Backend connection dropped".into(),
                recoverable: false,
            })],
        ];
        let mock_responses = create_mock_responses(&configs, responses);
        let factory = create_mock_provider_factory(mock_responses);

        let client = FailoverClient::new(configs, factory);

        // When: Making a call
        let result = client.get_block_number().await;

        // Then: The call should fail and the client should be permanently failed
        assert!(result.is_err());
        assert!(client.permanently_failed.load(Ordering::Relaxed));

        // And subsequent calls should immediately fail without attempting connection
        let second_result = client.get_block_number().await;
        assert!(second_result.is_err());
    }

    #[tokio::test]
    async fn test_request_errors_dont_trigger_failover() {
        // Given: A client with a provider that returns a request error (non-connection)
        let configs = vec![ProviderConfig::new("mock://p1")];
        let responses = vec![vec![
            Err(EthError::TxExecution("Insufficient funds".into())),
            Ok(MockResponseType::BlockNumber(100)), // This should be used on the second call
        ]];
        let mock_responses = create_mock_responses(&configs, responses);
        let factory = create_mock_provider_factory(mock_responses);

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
