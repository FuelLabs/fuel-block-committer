use std::{
    collections::VecDeque,
    fmt::Display,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::Mutex;
use tracing::warn;

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
    active_provider: Arc<Mutex<ProviderHandle<P>>>,
}

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
            "Transient connection error detected: {reason} (Count: {current_count}/{TRANSIENT_ERROR_THRESHOLD})",
        );
    }

    pub fn is_healthy(&self) -> bool {
        self.health.transient_error_count.load(Ordering::Relaxed) < self.health.max_transient_errors
            || !self.health.permanently_failed.load(Ordering::Relaxed)
    }

    pub fn note_permanent_failure(&self, reason: impl Display) {
        warn!("Provider '{}' permanently failed: {reason}", self.name);
        self.health
            .permanently_failed
            .store(true, Ordering::Relaxed);
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
    async fn get_healthy_provider(&self) -> EthResult<ProviderHandle<P>> {
        let mut current_provider_handle_lock = self.active_provider.lock().await;

        if current_provider_handle_lock.is_healthy() {
            return Ok(current_provider_handle_lock.clone());
        }

        let mut configs = self.configs.lock().await;
        let provider_handle =
            connect_to_first_available_provider(&self.provider_factory, &mut configs).await?;
        *current_provider_handle_lock = provider_handle.clone();
        Ok(provider_handle)
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
        let provider = self.get_healthy_provider().await?;

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
) -> EthResult<ProviderHandle<P>> {
    while let Some(config) = configs.pop_front() {
        match (provider_factory)(&config).await {
            Ok(provider) => {
                let tracked = ProviderHandle::new(config.name, provider, TRANSIENT_ERROR_THRESHOLD);
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
