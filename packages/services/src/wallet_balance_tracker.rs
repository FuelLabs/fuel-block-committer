use std::collections::HashMap;

use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use ports::types::{Address, U256};

use super::Runner;
use crate::Result;

struct Balance {
    gauge: IntGauge,
    address: Address,
}

pub struct WalletBalanceTracker<Api> {
    api: Api,
    tracking: HashMap<String, Balance>,
}

impl<Api> WalletBalanceTracker<Api>
where
    Api: ports::l1::Api,
{
    pub fn new(api: Api) -> Self {
        Self {
            api,
            tracking: Default::default(),
        }
    }

    pub fn track_address(&mut self, name: &str, address: ::ports::types::Address) {
        self.tracking.insert(
            name.to_owned(),
            Balance {
                gauge: IntGauge::with_opts(
                    Opts::new("wallet_balance", "Wallet balance [gwei].")
                        .const_label("usage", name),
                )
                .expect("wallet balance metric to be correctly configured"),
                address,
            },
        );
    }

    pub async fn update_balance(&self) -> Result<()> {
        for balance_tracker in self.tracking.values() {
            let balance = self.api.balance(balance_tracker.address).await?;
            let balance_gwei = balance / U256::from(1_000_000_000);
            balance_tracker.gauge.set(balance_gwei.to::<i64>());
        }

        Ok(())
    }
}

impl<Api> RegistersMetrics for WalletBalanceTracker<Api> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        self.tracking
            .values()
            .map(|balance_tracker| Box::new(balance_tracker.gauge.clone()) as Box<dyn Collector>)
            .collect()
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
    use mockall::predicate::eq;
    use ports::{l1, types::Address};

    use super::*;

    #[tokio::test]
    async fn updates_metrics() {
        // given
        let address_1 = "0x0000000000000000000000000000000000000000"
            .parse()
            .unwrap();
        let address_2 = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();

        let eth_adapter = has_balances([
            (address_1, "500000000000000000000"),
            (address_2, "400000000000000000000"),
        ]);
        let registry = Registry::new();

        let mut sut = WalletBalanceTracker::new(eth_adapter);
        sut.track_address("something_1", address_1);
        sut.track_address("something_2", address_2);
        sut.register_metrics(&registry);

        // when
        sut.update_balance().await.unwrap();

        // then
        let metrics = registry.gather();

        for (expected_label_value, expected_balance) in [
            ("something_1", 500_000_000_000_f64),
            ("something_2", 400_000_000_000_f64),
        ] {
            let eth_balance_metric = metrics
                .iter()
                .filter(|metric_group| metric_group.get_name() == "wallet_balance")
                .flat_map(|metric_group| metric_group.get_metric())
                .filter(|metric| {
                    metric.get_label().iter().any(|label| {
                        label.get_name() == "usage" && (label.get_value() == expected_label_value)
                    })
                })
                .map(Metric::get_gauge)
                .next()
                .unwrap();

            assert_eq!(eth_balance_metric.get_value(), expected_balance);
        }
    }

    fn has_balances(
        expectations: impl IntoIterator<Item = (Address, &'static str)>,
    ) -> l1::MockApi {
        let mut eth_adapter = l1::MockApi::new();
        for (address, balance) in expectations {
            let balance = U256::from_str(balance).unwrap();

            eth_adapter
                .expect_balance()
                .with(eq(address))
                .return_once(move |_| Box::pin(async move { Ok(balance) }));
        }
        eth_adapter
    }
}
