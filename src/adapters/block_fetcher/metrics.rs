use prometheus::{IntCounter, Opts};

use crate::metrics::RegistersMetrics;

pub struct Metrics {
    pub fuel_network_errors: IntCounter,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        vec![Box::new(self.fuel_network_errors.clone())]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let fuel_network_errors = IntCounter::with_opts(Opts::new(
            "fuel_network_errors",
            "Number of network errors encountered while polling for a new Fuel block.",
        ))
        .unwrap();
        Self {
            fuel_network_errors,
        }
    }
}
