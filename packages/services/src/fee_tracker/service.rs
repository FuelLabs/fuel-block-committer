use std::{
    cmp::min,
    num::{NonZeroU32, NonZeroU64},
    ops::RangeInclusive,
};

use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use tracing::info;

use crate::{state_committer::service::SendOrWaitDecider, Error, Result, Runner};

use super::{
    fee_analytics::FeeAnalytics,
    port::l1::{Api, BlockFees, Fees},
};

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub sma_periods: SmaPeriods,
    pub fee_thresholds: FeeThresholds,
}

#[cfg(feature = "test-helpers")]
impl Default for Config {
    fn default() -> Self {
        Config {
            sma_periods: SmaPeriods {
                short: 1.try_into().expect("not zero"),
                long: 2.try_into().expect("not zero"),
            },
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                ..FeeThresholds::default()
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SmaPeriods {
    pub short: NonZeroU64,
    pub long: NonZeroU64,
}

#[derive(Debug, Clone, Copy)]
pub struct FeeThresholds {
    pub max_l2_blocks_behind: NonZeroU32,
    pub start_discount_percentage: Percentage,
    pub end_premium_percentage: Percentage,
    pub always_acceptable_fee: u128,
}

#[cfg(feature = "test-helpers")]
impl Default for FeeThresholds {
    fn default() -> Self {
        Self {
            max_l2_blocks_behind: NonZeroU32::MAX,
            start_discount_percentage: Percentage::ZERO,
            end_premium_percentage: Percentage::ZERO,
            always_acceptable_fee: u128::MAX,
        }
    }
}

impl<P: Api + Send + Sync> SendOrWaitDecider for FeeTracker<P> {
    async fn should_send_blob_tx(
        &self,
        num_blobs: u32,
        num_l2_blocks_behind: u32,
        at_l1_height: u64,
    ) -> Result<bool> {
        if self.too_far_behind(num_l2_blocks_behind) {
            info!("Sending because we've fallen behind by {} which is more than the configured maximum of {}", num_l2_blocks_behind, self.config.fee_thresholds.max_l2_blocks_behind);
            return Ok(true);
        }

        // opted out of validating that num_blobs <= 6, it's not this fn's problem if the caller
        // wants to send more than 6 blobs
        let last_n_blocks = |n| Self::last_n_blocks(at_l1_height, n);

        let short_term_sma = self
            .fee_analytics
            .calculate_sma(last_n_blocks(self.config.sma_periods.short))
            .await?;

        let long_term_sma = self
            .fee_analytics
            .calculate_sma(last_n_blocks(self.config.sma_periods.long))
            .await?;

        let short_term_tx_fee = Self::calculate_blob_tx_fee(num_blobs, &short_term_sma);

        if self.fee_always_acceptable(short_term_tx_fee) {
            info!("Sending because: short term price {} is deemed always acceptable since it is <= {}", short_term_tx_fee, self.config.fee_thresholds.always_acceptable_fee);
            return Ok(true);
        }

        let long_term_tx_fee = Self::calculate_blob_tx_fee(num_blobs, &long_term_sma);
        let max_upper_tx_fee = Self::calculate_max_upper_fee(
            &self.config.fee_thresholds,
            long_term_tx_fee,
            num_l2_blocks_behind,
        );

        let should_send = short_term_tx_fee < max_upper_tx_fee;

        if should_send {
            info!(
                "Sending because short term price {} is lower than the max upper fee {}",
                short_term_tx_fee, max_upper_tx_fee
            );
        } else {
            info!(
                "Not sending because short term price {} is higher than the max upper fee {}",
                short_term_tx_fee, max_upper_tx_fee
            );
        }

        Ok(should_send)
    }
}

#[derive(Default, Copy, Clone, Debug, PartialEq)]
pub struct Percentage(f64);

impl TryFrom<f64> for Percentage {
    type Error = Error;

    fn try_from(value: f64) -> std::result::Result<Self, Self::Error> {
        if value < 0. {
            return Err(Error::Other(format!("Invalid percentage value {value}")));
        }

        Ok(Self(value))
    }
}

impl From<Percentage> for f64 {
    fn from(value: Percentage) -> Self {
        value.0
    }
}

impl Percentage {
    pub const ZERO: Self = Percentage(0.);
    pub const PPM: u128 = 1_000_000;

    pub fn ppm(&self) -> u128 {
        (self.0 * 1_000_000.) as u128
    }
}

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

impl<P> RegistersMetrics for FeeTracker<P> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.metrics.current_blob_tx_fee.clone()),
            Box::new(self.metrics.short_term_blob_tx_fee.clone()),
            Box::new(self.metrics.long_term_blob_tx_fee.clone()),
        ]
    }
}

#[derive(Clone)]
pub struct FeeTracker<P> {
    fee_analytics: FeeAnalytics<P>,
    config: Config,
    metrics: Metrics,
}

impl<P: Api> FeeTracker<P> {
    fn too_far_behind(&self, num_l2_blocks_behind: u32) -> bool {
        num_l2_blocks_behind >= self.config.fee_thresholds.max_l2_blocks_behind.get()
    }

    fn fee_always_acceptable(&self, short_term_tx_fee: u128) -> bool {
        short_term_tx_fee <= self.config.fee_thresholds.always_acceptable_fee
    }

    fn calculate_max_upper_fee(
        fee_thresholds: &FeeThresholds,
        fee: u128,
        num_l2_blocks_behind: u32,
    ) -> u128 {
        let max_blocks_behind = u128::from(fee_thresholds.max_l2_blocks_behind.get());
        let blocks_behind = u128::from(num_l2_blocks_behind);

        debug_assert!(
            blocks_behind <= max_blocks_behind,
            "blocks_behind ({}) should not exceed max_blocks_behind ({}), it should have been handled earlier",
            blocks_behind,
            max_blocks_behind
        );

        let start_discount_ppm = min(
            fee_thresholds.start_discount_percentage.ppm(),
            Percentage::PPM,
        );
        let end_premium_ppm = fee_thresholds.end_premium_percentage.ppm();

        // 1. The highest we're initially willing to go: eg. 100% - 20% = 80%
        let base_multiplier = Percentage::PPM.saturating_sub(start_discount_ppm);

        // 2. How late are we: eg. late enough to add 25% to our base multiplier
        let premium_increment = Self::calculate_premium_increment(
            start_discount_ppm,
            end_premium_ppm,
            blocks_behind,
            max_blocks_behind,
        );

        // 3. Total multiplier consist of the base and the premium increment: eg. 80% + 25% = 105%
        let multiplier_ppm = min(
            base_multiplier.saturating_add(premium_increment),
            Percentage::PPM + end_premium_ppm,
        );

        // 3. Final fee: eg. 105% of the base fee
        fee.saturating_mul(multiplier_ppm)
            .saturating_div(Percentage::PPM)
    }

    fn calculate_premium_increment(
        start_discount_ppm: u128,
        end_premium_ppm: u128,
        blocks_behind: u128,
        max_blocks_behind: u128,
    ) -> u128 {
        let total_ppm = start_discount_ppm.saturating_add(end_premium_ppm);

        let proportion = if max_blocks_behind == 0 {
            0
        } else {
            blocks_behind
                .saturating_mul(Percentage::PPM)
                .saturating_div(max_blocks_behind)
        };

        total_ppm
            .saturating_mul(proportion)
            .saturating_div(Percentage::PPM)
    }

    // TODO: Segfault maybe dont leak so much eth abstractions
    fn calculate_blob_tx_fee(num_blobs: u32, fees: &Fees) -> u128 {
        const DATA_GAS_PER_BLOB: u128 = 131_072u128;
        const INTRINSIC_GAS: u128 = 21_000u128;

        let base_fee = INTRINSIC_GAS * fees.base_fee_per_gas;
        let blob_fee = fees.base_fee_per_blob_gas * num_blobs as u128 * DATA_GAS_PER_BLOB;

        base_fee + blob_fee + fees.reward
    }

    fn last_n_blocks(current_block: u64, n: NonZeroU64) -> RangeInclusive<u64> {
        current_block.saturating_sub(n.get().saturating_sub(1))..=current_block
    }

    pub async fn update_metrics(&self) -> Result<()> {
        let latest_fees = self.fee_analytics.latest_fees().await?;
        let short_term_sma = self
            .fee_analytics
            .calculate_sma(Self::last_n_blocks(
                latest_fees.height,
                self.config.sma_periods.short,
            ))
            .await?;

        let long_term_sma = self
            .fee_analytics
            .calculate_sma(Self::last_n_blocks(
                latest_fees.height,
                self.config.sma_periods.long,
            ))
            .await?;

        let calc_fee =
            |fees: &Fees| i64::try_from(Self::calculate_blob_tx_fee(6, fees)).unwrap_or(i64::MAX);

        self.metrics
            .current_blob_tx_fee
            .set(calc_fee(&latest_fees.fees));
        self.metrics
            .short_term_blob_tx_fee
            .set(calc_fee(&short_term_sma));
        self.metrics
            .long_term_blob_tx_fee
            .set(calc_fee(&long_term_sma));

        Ok(())
    }
}

impl<P> FeeTracker<P> {
    pub fn new(fee_provider: P, config: Config) -> Self {
        Self {
            fee_analytics: FeeAnalytics::new(fee_provider),
            config,
            metrics: Metrics::default(),
        }
    }
}

impl<P> Runner for FeeTracker<P>
where
    P: crate::fee_tracker::port::l1::Api + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        self.update_metrics().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::fee_tracker::port::l1::testing::ConstantFeeApi;

    use super::*;
    use test_case::test_case;

    #[test_case(
        // Test Case 1: No blocks behind, no discount or premium
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
        1000,
        0,
        1000;
        "No blocks behind, multiplier should be 100%"
    )]
    #[test_case(
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            start_discount_percentage: 0.20.try_into().unwrap(),
            end_premium_percentage: 0.25.try_into().unwrap(),
            always_acceptable_fee: 0,
        },
        2000,
        50,
        2050;
        "Half blocks behind with discount and premium"
    )]
    #[test_case(
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            start_discount_percentage: 0.25.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
        800,
        50,
        700;
        "Start discount only, no premium"
    )]
    #[test_case(
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            end_premium_percentage: 0.30.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
        1000,
        50,
        1150;
        "End premium only, no discount"
    )]
    #[test_case(
        // Test Case 8: High fee with premium
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            start_discount_percentage: 0.10.try_into().unwrap(),
            end_premium_percentage: 0.20.try_into().unwrap(),
            always_acceptable_fee: 0,
        },
        10_000,
        99,
        11970;
        "High fee with premium"
    )]
    #[test_case(
    FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        start_discount_percentage: 1.50.try_into().unwrap(), // 150%
        end_premium_percentage: 0.20.try_into().unwrap(),
        always_acceptable_fee: 0,
    },
    1000,
    1,
    12;
    "Discount exceeds 100%, should be capped to 100%"
)]
    fn test_calculate_max_upper_fee(
        fee_thresholds: FeeThresholds,
        fee: u128,
        num_l2_blocks_behind: u32,
        expected_max_upper_fee: u128,
    ) {
        let max_upper_fee = FeeTracker::<ConstantFeeApi>::calculate_max_upper_fee(
            &fee_thresholds,
            fee,
            num_l2_blocks_behind,
        );

        assert_eq!(
            max_upper_fee, expected_max_upper_fee,
            "Expected max_upper_fee to be {}, but got {}",
            expected_max_upper_fee, max_upper_fee
        );
    }
}
