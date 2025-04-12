use ::metrics::{
    RegistersMetrics,
    prometheus::{IntCounterVec, Opts, core::Collector},
};

#[derive(Clone)]
pub struct Metrics {
    pub eth_network_errors: IntCounterVec,
    pub eth_mempool_drops: IntCounterVec,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.eth_network_errors.clone()),
            Box::new(self.eth_mempool_drops.clone()),
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

        let eth_mempool_drops = IntCounterVec::new(
            Opts::new(
                "eth_mempool_drops",
                "Number of transactions dropped from mempool potentially caused by provider issues.",
            ),
            &["provider"],
        )
        .expect("eth_mempool_drops metric to be correctly configured");

        Self {
            eth_network_errors,
            eth_mempool_drops,
        }
    }
}
