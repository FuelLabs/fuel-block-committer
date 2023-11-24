use std::str::FromStr;

use ethers::{
    signers::{LocalWallet, Signer},
    types::{H160, U256},
};
use prometheus::{IntGauge, Opts};

use crate::{
    adapters::{ethereum_adapter::EthereumAdapter, runner::Runner},
    errors::Result,
    telemetry::RegistersMetrics,
};

pub struct WalletBalanceTracker {
    eth_adapter: Box<dyn EthereumAdapter>,
    metrics: Metrics,
    address: H160,
}

impl WalletBalanceTracker {
    pub fn new(adapter: impl EthereumAdapter + 'static, ethereum_wallet_key: &str) -> Self {
        let address = LocalWallet::from_str(ethereum_wallet_key)
            .expect("Valid eth key")
            .address();
        Self {
            eth_adapter: Box::new(adapter),
            metrics: Metrics::default(),
            address,
        }
    }

    pub async fn update_balance(&self) -> Result<()> {
        let balance = self.eth_adapter.balance(self.address).await?;

        let balance_gwei = balance / U256::from(1_000_000_000);
        self.metrics
            .eth_wallet_balance
            .set(balance_gwei.as_u64() as i64);

        Ok(())
    }
}

impl RegistersMetrics for WalletBalanceTracker {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        self.metrics.metrics()
    }
}

#[derive(Clone)]
struct Metrics {
    eth_wallet_balance: IntGauge,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        vec![Box::new(self.eth_wallet_balance.clone())]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let eth_wallet_balance = IntGauge::with_opts(Opts::new(
            "eth_wallet_balance",
            "Ethereum wallet balance [gwei].",
        ))
        .expect("eth_wallet_balance metric to be correctly configured");

        Self { eth_wallet_balance }
    }
}

#[async_trait::async_trait]
impl Runner for WalletBalanceTracker {
    async fn run(&mut self) -> Result<()> {
        self.update_balance().await
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;
    use prometheus::{proto::Metric, Registry};

    use super::*;
    use crate::adapters::ethereum_adapter::MockEthereumAdapter;

    #[tokio::test]
    async fn updates_metrics() {
        // given
        let eth_private_key = "0000000000000000000000000000000000000000000000000000000000000001";
        let eth_adapter = given_eth_adapter(
            "500000000000000000000",
            "7E5F4552091A69125d5DfCb7b8C2659029395Bdf",
        );
        let registry = Registry::new();

        let sut = WalletBalanceTracker::new(eth_adapter, eth_private_key);
        sut.register_metrics(&registry);

        // when
        sut.update_balance().await.unwrap();

        // then
        let metrics = registry.gather();
        let eth_balance_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "eth_wallet_balance")
            .and_then(|metric| metric.get_metric().first())
            .map(Metric::get_gauge)
            .unwrap();

        assert_eq!(eth_balance_metric.get_value(), 500_000_000_000_f64);
    }

    fn given_eth_adapter(wei_balance: &str, expected_addr: &str) -> MockEthereumAdapter {
        let addr = H160::from_str(expected_addr).unwrap();
        let balance = U256::from_dec_str(wei_balance).unwrap();

        let mut eth_adapter = MockEthereumAdapter::new();
        eth_adapter
            .expect_balance()
            .with(eq(addr))
            .return_once(move |_| Ok(balance));

        eth_adapter
    }
}
