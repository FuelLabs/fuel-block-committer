use metrics::HealthChecker;
use prometheus::Opts;

use metrics::Collector;

use metrics::RegistersMetrics;

use prometheus::IntCounter;

use crate::client::Client;

pub struct Metrics {
    pub fuel_network_errors: IntCounter,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![Box::new(self.fuel_network_errors.clone())]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let fuel_network_errors = IntCounter::with_opts(Opts::new(
            "fuel_network_errors",
            "Number of network errors encountered while polling for a new Fuel block.",
        ))
        .expect("fuel_network_errors metric to be correctly configured");
        Self {
            fuel_network_errors,
        }
    }
}

impl RegistersMetrics for Client {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        self.metrics.metrics()
    }
}

impl Client {
    pub fn connection_health_checker(&self) -> HealthChecker {
        self.health_tracker.tracker()
    }

    pub(crate) fn handle_network_error(&self) {
        self.health_tracker.note_failure();
        self.metrics.fuel_network_errors.inc();
    }

    pub(crate) fn handle_network_success(&self) {
        self.health_tracker.note_success();
    }
}
