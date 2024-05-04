use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use super::{HealthCheck, HealthChecker};

#[derive(Debug, Clone)]
pub struct ConnectionHealthTracker {
    // how many failures are needed before the connection is deemed unhealhty
    max_consecutive_failures: usize,
    // how many consecutive failures there currently are
    consecutive_failures: Arc<AtomicUsize>,
}

impl ConnectionHealthTracker {
    pub fn new(max_consecutive_failures: usize) -> Self {
        Self {
            max_consecutive_failures,
            consecutive_failures: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn note_failure(&self) {
        self.consecutive_failures.fetch_add(1, Ordering::SeqCst);
    }

    pub fn note_success(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
    }

    pub fn tracker(&self) -> HealthChecker {
        Box::new(self.clone())
    }
}

impl HealthCheck for ConnectionHealthTracker {
    fn healthy(&self) -> bool {
        self.consecutive_failures.load(Ordering::Relaxed) < self.max_consecutive_failures
    }
}
