use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::{HealthCheck, HealthChecker};

#[derive(Debug, Clone)]
pub struct ConnectionHealthTracker {
    // how many failures are needed before the connection is deemed unhealthy
    max_consecutive_failures: usize,
    // how many consecutive failures there currently are
    consecutive_failures: Arc<AtomicUsize>,
    permanent_failure: Arc<AtomicBool>,
}

impl ConnectionHealthTracker {
    pub fn new(max_consecutive_failures: usize) -> Self {
        Self {
            max_consecutive_failures,
            consecutive_failures: Arc::new(AtomicUsize::new(0)),
            permanent_failure: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn note_permanent_failure(&self) {
        self.permanent_failure.store(true, Ordering::SeqCst);
    }

    pub fn note_failure(&self) {
        if !self.permanent_failure.load(Ordering::Relaxed) {
            self.consecutive_failures.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn note_success(&self) {
        if !self.permanent_failure.load(Ordering::Relaxed) {
            self.consecutive_failures.store(0, Ordering::SeqCst);
        }
    }

    pub fn tracker(&self) -> HealthChecker {
        Box::new(self.clone())
    }
}

#[async_trait::async_trait]
impl HealthCheck for ConnectionHealthTracker {
    async fn healthy(&self) -> bool {
        if self.permanent_failure.load(Ordering::Relaxed) {
            return false;
        }

        self.consecutive_failures.load(Ordering::Relaxed) < self.max_consecutive_failures
    }
}
