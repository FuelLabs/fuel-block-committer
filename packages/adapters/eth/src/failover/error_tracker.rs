use std::{
    collections::VecDeque,
    fmt::Display,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use tracing::{error, warn};

use crate::Error as EthError;

/// Enum to classify error types for failover decisions
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ErrorClassification {
    /// Fatal error that should immediately trigger failover
    Fatal,
    /// Transient error that should trigger failover if threshold is exceeded
    Transient,
    /// Error in the request itself, not related to connection
    Other,
}

/// Configuration for provider health thresholds and error tolerance
#[derive(Debug, Clone, Copy)]
pub struct ProviderHealthThresholds {
    /// Maximum number of transient errors before considering a provider unhealthy
    pub transient_error_threshold: usize,
    /// Number of mempool drops to trigger unhealthy state
    pub mempool_drop_threshold: usize,
    /// Time window to consider for mempool drops
    pub mempool_drop_window: Duration,
}

pub fn classify_error(err: &EthError) -> ErrorClassification {
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

/// Tracks errors and failures for a provider
pub struct ErrorTracker {
    pub transient_error_count: AtomicUsize,
    pub permanently_failed: AtomicBool,
    // Track transactions that failed not due to network errors but due to mempool issues
    pub mempool_drop_window: tokio::sync::RwLock<VecDeque<Instant>>,
}

impl ErrorTracker {
    pub fn new() -> Self {
        Self {
            transient_error_count: AtomicUsize::new(0),
            permanently_failed: AtomicBool::new(false),
            mempool_drop_window: tokio::sync::RwLock::new(VecDeque::new()),
        }
    }

    pub fn reset_transient_error_count(&self) {
        self.transient_error_count.store(0, Ordering::Relaxed);
    }

    pub fn note_transient_error(&self, reason: impl Display, provider_name: &str) {
        let current_count = self.transient_error_count.fetch_add(1, Ordering::Relaxed) + 1;

        warn!(
            "Transient connection error detected on provider '{provider_name}': {reason} \
             (Count: {current_count})",
        );
    }

    pub fn note_permanent_failure(&self, reason: impl Display, provider_name: &str) {
        error!("Provider '{}' permanently failed: {reason}", provider_name);
        self.permanently_failed.store(true, Ordering::Relaxed);
    }

    pub async fn check_mempool_drop_threshold(
        &self,
        mempool_drop_threshold: usize,
        mempool_drop_window: Duration,
    ) -> bool {
        let failure_window = self.mempool_drop_window.read().await;
        let cutoff = Instant::now() - mempool_drop_window;

        // Since entries are ordered by timestamp, we can iterate from the back
        // and stop as soon as we find an entry outside the time window
        failure_window
            .iter()
            .rev()
            .take_while(|&&timestamp| timestamp >= cutoff)
            .count()
            >= mempool_drop_threshold
    }

    pub async fn note_mempool_drop(
        &self,
        reason: impl Display,
        provider_name: &str,
        time_window: Duration,
    ) {
        let mut failure_window = self.mempool_drop_window.write().await;
        let now = Instant::now();
        let cutoff = now - time_window;
        failure_window.retain(|&timestamp| timestamp >= cutoff);
        failure_window.push_back(now);

        warn!(
            "Transaction mempool drop detected on provider '{}': {}. (Current drop count: {})",
            provider_name,
            reason,
            failure_window.len()
        );
    }

    pub fn is_permanently_failed(&self) -> bool {
        self.permanently_failed.load(Ordering::Relaxed)
    }

    pub fn get_transient_error_count(&self) -> usize {
        self.transient_error_count.load(Ordering::Relaxed)
    }
}
