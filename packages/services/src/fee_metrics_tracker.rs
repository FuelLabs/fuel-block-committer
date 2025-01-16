pub mod service {
    use std::{num::NonZeroU64, ops::RangeInclusive};

    use metrics::{
        prometheus::{core::Collector, IntGauge, Opts},
        RegistersMetrics,
    };

    use crate::{
        fees::{Api, Fees},
        Result, Runner,
    };

    #[derive(Debug, Clone)]
    struct FeeMetrics {
        current: IntGauge,
    }

    impl Default for FeeMetrics {
        fn default() -> Self {
            let current = IntGauge::with_opts(Opts::new(
                "current_blob_tx_fee",
                "The current fee for a transaction with 6 blobs",
            ))
            .expect("metric config to be correct");

            Self { current }
        }
    }

    impl<P> RegistersMetrics for FeeMetricsTracker<P> {
        fn metrics(&self) -> Vec<Box<dyn Collector>> {
            vec![Box::new(self.metrics.current.clone())]
        }
    }

    #[derive(Clone)]
    pub struct FeeMetricsTracker<P> {
        fee_provider: P,
        metrics: FeeMetrics,
    }

    pub fn calculate_blob_tx_fee(num_blobs: u32, fees: &Fees) -> u128 {
        const DATA_GAS_PER_BLOB: u128 = 131_072u128;
        const INTRINSIC_GAS: u128 = 21_000u128;

        let base_fee = INTRINSIC_GAS.saturating_mul(fees.base_fee_per_gas);
        let blob_fee = fees
            .base_fee_per_blob_gas
            .saturating_mul(u128::from(num_blobs))
            .saturating_mul(DATA_GAS_PER_BLOB);
        let reward_fee = fees.reward.saturating_mul(INTRINSIC_GAS);

        base_fee.saturating_add(blob_fee).saturating_add(reward_fee)
    }

    const fn last_n_blocks(current_block: u64, n: NonZeroU64) -> RangeInclusive<u64> {
        current_block.saturating_sub(n.get().saturating_sub(1))..=current_block
    }

    impl<P> FeeMetricsTracker<P> {
        pub fn new(fee_provider: P) -> Self {
            Self {
                fee_provider,
                metrics: FeeMetrics::default(),
            }
        }
    }

    impl<P: Api> FeeMetricsTracker<P> {
        pub async fn update_metrics(&self) -> Result<()> {
            let current_block = self.fee_provider.current_height().await?;
            let tx_fees_for_last_n_blocks = |n| async move {
                let fees = self
                    .fee_provider
                    .fees(last_n_blocks(current_block, n))
                    .await?
                    .mean();

                Result::Ok(i64::try_from(calculate_blob_tx_fee(6, &fees)).unwrap_or(i64::MAX))
            };

            let current = tx_fees_for_last_n_blocks(1.try_into().expect("not zero")).await?;

            self.metrics.current.set(current);

            Ok(())
        }
    }

    impl<P> Runner for FeeMetricsTracker<P>
    where
        P: Api + Send + Sync,
    {
        async fn run(&mut self) -> Result<()> {
            self.update_metrics().await?;
            Ok(())
        }
    }
}
