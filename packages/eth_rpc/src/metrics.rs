use metrics::{
    prometheus::{core::Collector, IntCounter, Opts},
    RegistersMetrics,
};

#[derive(Clone)]
pub struct Metrics {
    pub(crate) eth_network_errors: IntCounter,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![Box::new(self.eth_network_errors.clone())]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let eth_network_errors = IntCounter::with_opts(Opts::new(
            "eth_network_errors",
            "Number of network errors encountered while running Ethereum RPCs.",
        ))
        .expect("eth_network_errors metric to be correctly configured");

        Self { eth_network_errors }
    }
}
