use ::metrics::{
    RegistersMetrics,
    prometheus::{IntCounterVec, Opts, core::Collector},
};

#[derive(Clone)]
pub struct Metrics {
    pub eth_network_errors: IntCounterVec,
    pub eth_tx_failures: IntCounterVec,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.eth_network_errors.clone()),
            Box::new(self.eth_tx_failures.clone()),
        ]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let eth_network_errors = IntCounterVec::new(
            Opts::new(
                "eth_network_errors",
                "Number of network errors encountered while running Ethereum RPCs.",
            ),
            &["provider"],
        )
        .expect("eth_network_errors metric to be correctly configured");

        let eth_tx_failures = IntCounterVec::new(
            Opts::new(
                "eth_tx_failures",
                "Number of transaction failures potentially caused by provider issues.",
            ),
            &["provider"],
        )
        .expect("eth_tx_failures metric to be correctly configured");

        Self {
            eth_network_errors,
            eth_tx_failures,
        }
    }
}
