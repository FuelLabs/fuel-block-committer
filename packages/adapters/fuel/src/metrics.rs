use metrics::{
    RegistersMetrics,
    prometheus::{IntCounter, IntGauge, Opts, core::Collector},
};

#[derive(Clone)]
pub struct Metrics {
    pub fuel_network_errors: IntCounter,
    pub fuel_height: IntGauge,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.fuel_network_errors.clone()),
            Box::new(self.fuel_height.clone()),
        ]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let fuel_network_errors = IntCounter::with_opts(Opts::new(
            "fuel_network_errors",
            "Number of network errors encountered while polling for a new Fuel block.",
        ))
        .expect("fuel_network_errors metric to be correctly configured");
        let fuel_height =
            IntGauge::with_opts(Opts::new("fuel_height", "Latest block height in Fuel"))
                .expect("fuel_height metric to be correctly configured");

        Self {
            fuel_network_errors,
            fuel_height,
        }
    }
}
