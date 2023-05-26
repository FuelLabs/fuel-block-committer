use serde::Serialize;

use crate::health_check::{HealthCheck, HealthChecker};

#[derive(Debug, Serialize)]
pub struct HealthReport {
    fuel_connection_up: bool,
    // TODO
    // ethereum_connection_up: bool,
}

impl HealthReport {
    pub fn healthy(&self) -> bool {
        self.fuel_connection_up
    }
}

pub struct HealthReporter {
    fuel_connection: HealthChecker,
}

impl HealthReporter {
    pub fn new(fuel_health_check: HealthChecker) -> Self {
        Self {
            fuel_connection: fuel_health_check,
        }
    }

    pub fn report(&self) -> HealthReport {
        HealthReport {
            fuel_connection_up: self.fuel_connection.healthy(),
        }
    }
}
