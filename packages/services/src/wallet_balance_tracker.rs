use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use ports::types::U256;

use super::Runner;
use crate::Result;

pub struct WalletBalanceTracker<Api> {
    api: Api,
    metrics: Metrics,
}

impl<Api> WalletBalanceTracker<Api>
where
    Api: ports::l1::Api,
{
    pub fn new(api: Api) -> Self {
        Self {
            api,
            metrics: Metrics::default(),
        }
    }

    pub async fn update_balance(&self) -> Result<()> {
        let balance = self.api.balance().await?;

        let balance_gwei = balance / U256::from(1_000_000_000);
        self.metrics
            .eth_wallet_balance
            .set(balance_gwei.to::<i64>());

        Ok(())
    }
}

impl<Api> RegistersMetrics for WalletBalanceTracker<Api> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        self.metrics.metrics()
    }
}

#[derive(Clone)]
struct Metrics {
    eth_wallet_balance: IntGauge,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
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

impl<Api> Runner for WalletBalanceTracker<Api>
where
    Api: Send + Sync + ports::l1::Api,
{
    async fn run(&mut self) -> Result<()> {
        self.update_balance().await
    }
}

#[cfg(test)]
mod tests {

    use std::str::FromStr;

    use metrics::prometheus::{proto::Metric, Registry};
    use ports::l1;

    use super::*;

    #[tokio::test]
    async fn updates_metrics() {
        // given
        let eth_adapter = given_l1_api("500000000000000000000");
        let registry = Registry::new();

        let sut = WalletBalanceTracker::new(eth_adapter);
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

    fn given_l1_api(wei_balance: &str) -> l1::MockApi {
        let balance = U256::from_str(wei_balance).unwrap();

        let mut eth_adapter = l1::MockApi::new();
        eth_adapter
            .expect_balance()
            .return_once(move || Box::pin(async move { Ok(balance) }));

        eth_adapter
    }
}
