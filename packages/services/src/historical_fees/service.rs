use std::{
    num::NonZeroU64,
    ops::RangeInclusive,
};

use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};

use super::port::l1::{Api, BlockFees, Fees};
use crate::{Error, Result, Runner};

#[derive(Debug, Clone)]
struct Metrics {
    current_blob_tx_fee: IntGauge,
    short_term_blob_tx_fee: IntGauge,
    long_term_blob_tx_fee: IntGauge,
}

impl Default for Metrics {
    fn default() -> Self {
        let current_blob_tx_fee = IntGauge::with_opts(Opts::new(
            "current_blob_tx_fee",
            "The current fee for a transaction with 6 blobs",
        ))
        .expect("metric config to be correct");

        let short_term_blob_tx_fee = IntGauge::with_opts(Opts::new(
            "short_term_blob_tx_fee",
            "The short term fee for a transaction with 6 blobs",
        ))
        .expect("metric config to be correct");

        let long_term_blob_tx_fee = IntGauge::with_opts(Opts::new(
            "long_term_blob_tx_fee",
            "The long term fee for a transaction with 6 blobs",
        ))
        .expect("metric config to be correct");

        Self {
            current_blob_tx_fee,
            short_term_blob_tx_fee,
            long_term_blob_tx_fee,
        }
    }
}

impl<P> RegistersMetrics for HistoricalFees<P> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.metrics.current_blob_tx_fee.clone()),
            Box::new(self.metrics.short_term_blob_tx_fee.clone()),
            Box::new(self.metrics.long_term_blob_tx_fee.clone()),
        ]
    }
}

#[derive(Clone)]
pub struct HistoricalFees<P> {
    fee_provider: P,
    metrics: Metrics,
}

#[derive(Debug, Clone, Copy)]
pub struct SmaPeriods {
    pub short: NonZeroU64,
    pub long: NonZeroU64,
}

pub fn calculate_blob_tx_fee(num_blobs: u32, fees: &Fees) -> u128 {
    const DATA_GAS_PER_BLOB: u128 = 131_072u128;
    const INTRINSIC_GAS: u128 = 21_000u128;

    let base_fee = INTRINSIC_GAS.saturating_mul(fees.base_fee_per_gas.get());
    let blob_fee = fees
        .base_fee_per_blob_gas
        .get()
        .saturating_mul(u128::from(num_blobs))
        .saturating_mul(DATA_GAS_PER_BLOB);
    let reward_fee = fees.reward.get().saturating_mul(INTRINSIC_GAS);

    base_fee.saturating_add(blob_fee).saturating_add(reward_fee)
}

impl<P: Api> HistoricalFees<P> {
    pub async fn calculate_sma(&self, block_range: RangeInclusive<u64>) -> crate::Result<Fees> {
        let fees = self.fee_provider.fees(block_range.clone()).await?;

        let received_height_range = fees.height_range();
        if received_height_range != block_range {
            return Err(Error::from(format!(
                "fees received from the adapter({received_height_range:?}) don't cover the requested range ({block_range:?})"
            )));
        }

        Ok(fees.mean())
    }

    pub async fn latest_fees(&self) -> crate::Result<BlockFees> {
        let height = self.fee_provider.current_height().await?;

        let fee = self
            .fee_provider
            .fees(height..=height)
            .await?
            .into_iter()
            .next()
            .expect("sequential fees guaranteed not empty");

        Ok(fee)
    }
}

fn last_n_blocks(current_block: u64, n: NonZeroU64) -> RangeInclusive<u64> {
    current_block.saturating_sub(n.get().saturating_sub(1))..=current_block
}

impl<P> HistoricalFees<P> {
    pub fn new(fee_provider: P) -> Self {
        Self {
            fee_provider,
            metrics: Metrics::default(),
        }
    }

    pub fn fee_metrics_updater(&self, periods: SmaPeriods) -> FeeMetricsUpdater<P>
    where
        Self: Clone,
    {
        FeeMetricsUpdater {
            historical_fees: self.clone(),
            sma_periods: periods,
        }
    }
}

pub struct FeeMetricsUpdater<P> {
    historical_fees: HistoricalFees<P>,
    sma_periods: SmaPeriods,
}

impl<P> FeeMetricsUpdater<P> {
    pub fn new(historical_fees: HistoricalFees<P>, sma_periods: SmaPeriods) -> Self {
        Self {
            historical_fees,
            sma_periods,
        }
    }
}

impl<P: Api> FeeMetricsUpdater<P> {
    pub async fn update_metrics(&self) -> Result<()> {
        let metrics_sma = self.sma_periods;
        let latest_fees = self.historical_fees.latest_fees().await?;
        let short_term_sma = self
            .historical_fees
            .calculate_sma(last_n_blocks(latest_fees.height, metrics_sma.short))
            .await?;

        let long_term_sma = self
            .historical_fees
            .calculate_sma(last_n_blocks(latest_fees.height, metrics_sma.long))
            .await?;

        let calc_fee =
            |fees: &Fees| i64::try_from(calculate_blob_tx_fee(6, fees)).unwrap_or(i64::MAX);

        self.historical_fees
            .metrics
            .current_blob_tx_fee
            .set(calc_fee(&latest_fees.fees));
        self.historical_fees
            .metrics
            .short_term_blob_tx_fee
            .set(calc_fee(&short_term_sma));
        self.historical_fees
            .metrics
            .long_term_blob_tx_fee
            .set(calc_fee(&long_term_sma));

        Ok(())
    }
}

impl<P> Runner for FeeMetricsUpdater<P>
where
    P: Api + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        self.update_metrics().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    

    use super::*;
    use crate::historical_fees::port::l1::{testing, BlockFees};

    #[tokio::test]
    async fn calculates_sma_correctly_for_last_1_block() {
        // given
        let fees_provider = testing::PreconfiguredFeeApi::new(testing::incrementing_fees(5));
        let sut = HistoricalFees::new(fees_provider);

        // when
        let sma = sut.calculate_sma(4..=4).await.unwrap();

        // then
        assert_eq!(sma.base_fee_per_gas, 6.try_into().unwrap());
        assert_eq!(sma.reward, 6.try_into().unwrap());
        assert_eq!(sma.base_fee_per_blob_gas, 6.try_into().unwrap());
    }

    #[tokio::test]
    async fn calculates_sma_correctly_for_last_5_blocks() {
        // given
        let fees_provider = testing::PreconfiguredFeeApi::new(testing::incrementing_fees(5));
        let sut = HistoricalFees::new(fees_provider);

        // when
        let sma = sut.calculate_sma(0..=4).await.unwrap();

        // then
        let mean = ((5 + 4 + 3 + 2 + 1) / 5).try_into().unwrap();
        assert_eq!(sma.base_fee_per_gas, mean);
        assert_eq!(sma.reward, mean);
        assert_eq!(sma.base_fee_per_blob_gas, mean);
    }

    #[tokio::test]
    async fn errors_out_if_returned_fees_are_not_complete() {
        // given
        let mut fees = testing::incrementing_fees(5);
        fees.remove(&4);
        let fees_provider = testing::PreconfiguredFeeApi::new(fees);
        let sut = HistoricalFees::new(fees_provider);

        // when
        let err = sut
            .calculate_sma(0..=4)
            .await
            .expect_err("should have failed because returned fees are not complete");

        // then
        assert_eq!(
            err.to_string(),
            "fees received from the adapter(0..=3) don't cover the requested range (0..=4)"
        );
    }

    #[tokio::test]
    async fn latest_fees_on_fee_analytics() {
        // given
        let fees_map = testing::incrementing_fees(5);
        let fees_provider = testing::PreconfiguredFeeApi::new(fees_map.clone());
        let sut = HistoricalFees::new(fees_provider);
        let height = 4;

        // when
        let fee = sut.latest_fees().await.unwrap();

        // then
        let expected_fee = BlockFees {
            height,
            fees: Fees {
                base_fee_per_gas: 5.try_into().unwrap(),
                reward: 5.try_into().unwrap(),
                base_fee_per_blob_gas: 5.try_into().unwrap(),
            },
        };
        assert_eq!(
            fee, expected_fee,
            "Fee at height {height} should be {expected_fee:?}"
        );
    }

    // fn calculate_tx_fee(fees: &Fees) -> u128 {
    //     21_000 * fees.base_fee_per_gas + fees.reward + 6 * fees.base_fee_per_blob_gas * 131_072
    // }
    //
    // fn save_tx_fees(tx_fees: &[(u64, u128)], path: &str) {
    //     let mut csv_writer =
    //         csv::Writer::from_path(PathBuf::from("/home/segfault_magnet/grafovi/").join(path))
    //             .unwrap();
    //     csv_writer
    //         .write_record(["height", "tx_fee"].iter())
    //         .unwrap();
    //     for (height, fee) in tx_fees {
    //         csv_writer
    //             .write_record([height.to_string(), fee.to_string()])
    //             .unwrap();
    //     }
    //     csv_writer.flush().unwrap();
    // }

    // #[tokio::test]
    // async fn something() {
    //     let client = make_pub_eth_client().await;
    //     use services::fee_analytics::port::l1::FeesProvider;
    //
    //     let current_block_height = 21408300;
    //     let starting_block_height = current_block_height - 48 * 3600 / 12;
    //     let data = client
    //         .fees(starting_block_height..=current_block_height)
    //         .await
    //         .into_iter()
    //         .collect::<Vec<_>>();
    //
    //     let fee_lookup = data
    //         .iter()
    //         .map(|b| (b.height, b.fees))
    //         .collect::<HashMap<_, _>>();
    //
    //     let short_sma = 25u64;
    //     let long_sma = 900;
    //
    //     let current_tx_fees = data
    //         .iter()
    //         .map(|b| (b.height, calculate_tx_fee(&b.fees)))
    //         .collect::<Vec<_>>();
    //
    //     save_tx_fees(&current_tx_fees, "current_fees.csv");
    //
    //     let local_client = TestFeesProvider::new(data.clone().into_iter().map(|e| (e.height, e.fees)));
    //     let fee_analytics = FeeAnalytics::new(local_client.clone());
    //
    //     let mut short_sma_tx_fees = vec![];
    //     for height in (starting_block_height..=current_block_height).skip(short_sma as usize) {
    //         let fees = fee_analytics
    //             .calculate_sma(height - short_sma..=height)
    //             .await;
    //
    //         let tx_fee = calculate_tx_fee(&fees);
    //
    //         short_sma_tx_fees.push((height, tx_fee));
    //     }
    //     save_tx_fees(&short_sma_tx_fees, "short_sma_fees.csv");
    //
    //     let decider = SendOrWaitDecider::new(
    //         FeeAnalytics::new(local_client.clone()),
    //         services::state_committer::fee_optimization::Config {
    //             sma_periods: services::state_committer::fee_optimization::SmaBlockNumPeriods {
    //                 short: short_sma,
    //                 long: long_sma,
    //             },
    //             fee_thresholds: Feethresholds {
    //                 max_l2_blocks_behind: 43200 * 3,
    //                 start_discount_percentage: 0.2,
    //                 end_premium_percentage: 0.2,
    //                 always_acceptable_fee: 1000000000000000u128,
    //             },
    //         },
    //     );
    //
    //     let mut decisions = vec![];
    //     let mut long_sma_tx_fees = vec![];
    //
    //     for height in (starting_block_height..=current_block_height).skip(long_sma as usize) {
    //         let fees = fee_analytics
    //             .calculate_sma(height - long_sma..=height)
    //             .await;
    //         let tx_fee = calculate_tx_fee(&fees);
    //         long_sma_tx_fees.push((height, tx_fee));
    //
    //         if decider
    //             .should_send_blob_tx(
    //                 6,
    //             Context {
    //                 at_l1_height: height,
    //                 num_l2_blocks_behind: (height - starting_block_height) * 12,
    //             },
    //         )
    //         .await
    //     {
    //         let current_fees = fee_lookup.get(&height).unwrap();
    //         let current_tx_fee = calculate_tx_fee(current_fees);
    //         decisions.push((height, current_tx_fee));
    //     }
    // }
    //
    // save_tx_fees(&long_sma_tx_fees, "long_sma_fees.csv");
    // save_tx_fees(&decisions, "decisions.csv");
    // }
}
