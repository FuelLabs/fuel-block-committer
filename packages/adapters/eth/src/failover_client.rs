use std::{
    collections::VecDeque,
    fmt::Display,
    ops::RangeInclusive,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use tokio::sync::Mutex;
use tracing::warn;

// Suppose these come from your own crates:
use crate::error::{Error as EthError, Result as EthResult};
use crate::provider::{L1Provider, ProviderConfig, ProviderFactory};
use alloy::primitives::Address;
use alloy::rpc::types::FeeHistory;
use services::state_committer::port::l1::Priority;
use services::types::{
    BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx, NonEmpty, TransactionResponse, U256,
};

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
    // Factory to create new provider instances
    provider_factory: ProviderFactory<P>,
    shared_state: Arc<Mutex<SharedState<P>>>,
}

/// The combined, shared state of this client:
/// - The deque of provider configs
/// - The currently active provider handle
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
            "Transient connection error detected: {reason} (Count: {current_count}/{TRANSIENT_ERROR_THRESHOLD})",
        );
    }

    pub fn is_healthy(&self) -> bool {
        // If the provider is marked permanently failed, it is not healthy.
        if self.health.permanently_failed.load(Ordering::Relaxed) {
            return false;
        }

        // If the transient error count is below threshold, it’s still considered healthy.
        self.health.transient_error_count.load(Ordering::Relaxed) < self.health.max_transient_errors
    }

    pub fn note_permanent_failure(&self, reason: impl Display) {
        warn!("Provider '{}' permanently failed: {reason}", self.name);
        self.health
            .permanently_failed
            .store(true, Ordering::Relaxed);
    }
}

impl<P> FailoverClient<P> {
    /// Create a new FailoverClient with the given provider configurations and factory.
    /// This attempts to connect to the first available provider and stores both
    /// the `configs` and the `active_provider` together in `SharedState`.
    pub async fn connect(
        provider_configs: Vec<ProviderConfig>,
        provider_factory: ProviderFactory<P>,
    ) -> EthResult<Self> {
        let mut configs = VecDeque::from(provider_configs);
        // Attempt to connect right away
        let provider_handle =
            connect_to_first_available_provider(&provider_factory, &mut configs).await?;

        // Wrap it all in a single Mutex
        let shared_state = SharedState {
            configs,
            active_provider: provider_handle,
        };

        Ok(Self {
            provider_factory,
            shared_state: Arc::new(Mutex::new(shared_state)),
        })
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

    /// Retrieves the currently active provider if it's healthy, or tries to failover to the next.
    async fn get_healthy_provider(&self) -> EthResult<ProviderHandle<P>> {
        // Lock the entire shared state
        let mut state = self.shared_state.lock().await;

        // Check if the active provider is healthy
        if state.active_provider.is_healthy() {
            return Ok(state.active_provider.clone());
        }

        // If not healthy, attempt to connect to another provider
        let handle =
            connect_to_first_available_provider(&self.provider_factory, &mut state.configs).await?;

        // Replace the active provider with our newly connected one
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
        F: FnOnce(Arc<P>) -> Fut + Send + Sync,
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

#[async_trait::async_trait]
impl<P: L1Provider> L1Provider for FailoverClient<P> {
    async fn get_block_number(&self) -> EthResult<u64>
    where
        P: L1Provider,
    {
        self.execute_operation(|provider| async move { provider.get_block_number().await })
            .await
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> EthResult<Option<TransactionResponse>>
    where
        P: L1Provider,
    {
        self.execute_operation(move |provider| async move {
            provider.get_transaction_response(tx_hash).await
        })
        .await
    }

    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> EthResult<bool>
    where
        P: L1Provider,
    {
        self.execute_operation(
            move |provider| async move { provider.is_squeezed_out(tx_hash).await },
        )
        .await
    }

    async fn balance(&self, address: Address) -> EthResult<U256>
    where
        P: L1Provider,
    {
        self.execute_operation(move |provider| async move { provider.balance(address).await })
            .await
    }

    async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
        reward_percentiles: &[f64],
    ) -> EthResult<FeeHistory>
    where
        P: L1Provider,
    {
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
    ) -> EthResult<(L1Tx, FragmentsSubmitted)>
    where
        P: L1Provider,
    {
        self.execute_operation(move |provider| async move {
            provider
                .submit_state_fragments(fragments.clone(), previous_tx.clone(), priority)
                .await
        })
        .await
    }

    async fn submit(&self, hash: [u8; 32], height: u32) -> EthResult<BlockSubmissionTx>
    where
        P: L1Provider,
    {
        self.execute_operation(move |provider| async move { provider.submit(hash, height).await })
            .await
    }
}

/// Attempts to connect to the first available provider, removing each config from
/// the front of the deque if it fails.
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
