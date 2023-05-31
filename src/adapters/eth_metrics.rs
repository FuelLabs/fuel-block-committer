use prometheus::{IntCounter, IntGauge, Opts};

use crate::telemetry::RegistersMetrics;

#[derive(Clone)]
pub struct EthMetrics {
    pub eth_network_errors: IntCounter,
    pub eth_wallet_balance: IntGauge,
}

impl RegistersMetrics for EthMetrics {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        vec![
            Box::new(self.eth_network_errors.clone()),
            Box::new(self.eth_wallet_balance.clone()),
        ]
    }
}

impl Default for EthMetrics {
    fn default() -> Self {
        let eth_network_errors = IntCounter::with_opts(Opts::new(
            "eth_network_errors",
            "Number of network errors encountered while running Ethereum RPCs.",
        ))
        .expect("eth_network_errors metric to be correctly configured");

        let eth_wallet_balance = IntGauge::with_opts(Opts::new(
            "eth_wallet_balance",
            "Ethereum wallet balance [gwei].",
        ))
        .expect("eth_wallet_balance metric to be correctly configured");

        Self {
            eth_network_errors,
            eth_wallet_balance,
        }
    }
}
