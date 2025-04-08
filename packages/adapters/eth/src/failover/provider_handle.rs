use std::{fmt::Display, sync::Arc, time::Duration};

use tracing::debug;

use super::{ProviderHealthThresholds, error_tracker::ErrorTracker};

/// Holds an actual provider along with error tracking info (transient errors, etc.)
pub struct ProviderHandle<P> {
    pub name: String,
    pub provider: Arc<P>,
    error_tracker: Arc<ErrorTracker>,
}

impl<P> Clone for ProviderHandle<P> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            provider: Arc::clone(&self.provider),
            error_tracker: Arc::clone(&self.error_tracker),
        }
    }
}

impl<P> ProviderHandle<P> {
    pub fn new(name: String, provider: Arc<P>) -> Self {
        let error_tracker = ErrorTracker::new();

        Self {
            name,
            provider,
            error_tracker: Arc::new(error_tracker),
        }
    }

    pub fn reset_transient_error_count(&self) {
        self.error_tracker.reset_transient_error_count();
    }

    pub fn note_transient_error(&self, reason: impl Display, transient_error_threshold: usize) {
        self.error_tracker
            .note_transient_error(reason, &self.name, transient_error_threshold);
    }

    pub fn note_permanent_failure(&self, reason: impl Display) {
        self.error_tracker
            .note_permanent_failure(reason, &self.name);
    }

    pub async fn is_healthy(&self, health_thresholds: &ProviderHealthThresholds) -> bool {
        if self.error_tracker.is_permanently_failed() {
            return false;
        }

        let transient_error_count = self.error_tracker.get_transient_error_count();
        if transient_error_count >= health_thresholds.transient_error_threshold {
            return false;
        }

        // Check transaction failures within time window
        let tx_failures_exceed_threshold = self
            .error_tracker
            .check_tx_failure_threshold(
                health_thresholds.tx_failure_threshold,
                health_thresholds.tx_failure_time_window,
            )
            .await;

        if tx_failures_exceed_threshold {
            return false;
        }

        debug!("Provider '{}' is healthy", self.name);
        true
    }

    pub async fn note_tx_failure(&self, reason: impl Display, time_window: Duration) {
        self.error_tracker
            .note_tx_failure(reason, &self.name, time_window)
            .await;
    }
}
