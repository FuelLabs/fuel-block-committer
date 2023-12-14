use std::{
    borrow::BorrowMut,
    sync::{Arc, Mutex},
};

use crate::telemetry::{HealthCheck, HealthChecker};

#[derive(Debug, Clone)]
pub struct ConnectionHealthTracker {
    // how many failures are needed before the connection is deemed unhealhty
    max_consecutive_failures: usize,
    // how many consecutive failures there currently are
    consecutive_failures: Arc<Mutex<usize>>,
}

impl ConnectionHealthTracker {
    pub fn new(max_consecutive_failures: usize) -> Self {
        Self {
            max_consecutive_failures,
            consecutive_failures: Arc::new(Mutex::new(0)),
        }
    }

    pub fn note_failure(&self) {
        **self.acquire_consecutive_failures().borrow_mut() += 1;
    }

    pub fn note_success(&self) {
        **self.acquire_consecutive_failures().borrow_mut() = 0;
    }

    fn acquire_consecutive_failures(&self) -> std::sync::MutexGuard<usize> {
        self.consecutive_failures
            .lock()
            .expect("no need to handle poisoning since lock duration is short and no panics occur")
    }

    pub fn tracker(&self) -> HealthChecker {
        Box::new(self.clone())
    }
}

impl HealthCheck for ConnectionHealthTracker {
    fn healthy(&self) -> bool {
        *self.acquire_consecutive_failures() < self.max_consecutive_failures
    }
}
