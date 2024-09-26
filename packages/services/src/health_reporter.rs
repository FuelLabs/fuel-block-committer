use metrics::HealthChecker;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct HealthReport {
    fuel_connection_up: bool,
    eth_connection_healthy: bool,
}

impl HealthReport {
    pub fn is_healthy(&self) -> bool {
        self.fuel_connection_up && self.eth_connection_healthy
    }
}

pub struct HealthReporter {
    fuel_connection: HealthChecker,
    eth_connection: HealthChecker,
}

impl HealthReporter {
    #[must_use]
    pub fn new(fuel_health_check: HealthChecker, eth_health_check: HealthChecker) -> Self {
        Self {
            fuel_connection: fuel_health_check,
            eth_connection: eth_health_check,
        }
    }

    #[must_use]
    pub fn generate_report(&self) -> HealthReport {
        HealthReport {
            fuel_connection_up: self.fuel_connection.healthy(),
            eth_connection_healthy: self.eth_connection.healthy(),
        }
    }
}
