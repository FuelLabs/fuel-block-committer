use std::{
    cmp::min,
    num::{NonZeroU32, NonZeroU64},
    ops::RangeInclusive,
};

use tracing::info;

use crate::{
    historical_fees::{
        self,
        service::{HistoricalFees, SmaPeriods},
    },
    Error, Result,
};

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub sma_periods: SmaPeriods,
    pub fee_thresholds: FeeThresholds,
}

#[cfg(feature = "test-helpers")]
impl Default for Config {
    fn default() -> Self {
        Self {
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
pub struct FeeThresholds {
    pub max_l2_blocks_behind: NonZeroU32,
    // TODO: segfault a better way to express this
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

pub struct SmaFeeAlgo<P> {
    historical_fees: HistoricalFees<P>,
    config: Config,
}

impl<P> SmaFeeAlgo<P> {
    pub fn new(historical_fees: HistoricalFees<P>, config: Config) -> Self {
        Self {
            historical_fees,
            config,
        }
    }

    fn too_far_behind(&self, num_l2_blocks_behind: u32) -> bool {
        num_l2_blocks_behind >= self.config.fee_thresholds.max_l2_blocks_behind.get()
    }

    fn fee_always_acceptable(&self, short_term_tx_fee: u128) -> bool {
        short_term_tx_fee <= self.config.fee_thresholds.always_acceptable_fee
    }
}

fn last_n_blocks(current_block: u64, n: NonZeroU64) -> RangeInclusive<u64> {
    current_block.saturating_sub(n.get().saturating_sub(1))..=current_block
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
    let premium_increment = calculate_premium_increment(
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

    info!("start_discount_ppm: {start_discount_ppm}, end_premium_ppm: {end_premium_ppm}, base_multiplier: {base_multiplier}, premium_increment: {premium_increment}, multiplier_ppm: {multiplier_ppm}");

    // 3. Final fee: eg. 105% of the base fee
    fee.saturating_mul(multiplier_ppm)
        .saturating_div(Percentage::PPM)
}

impl<P> SmaFeeAlgo<P>
where
    P: historical_fees::port::l1::Api + Send + Sync,
{
    pub async fn fees_acceptable(
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
        let last_n_blocks = |n| last_n_blocks(at_l1_height, n);

        let short_term_sma = self
            .historical_fees
            .calculate_sma(last_n_blocks(self.config.sma_periods.short))
            .await?;

        let long_term_sma = self
            .historical_fees
            .calculate_sma(last_n_blocks(self.config.sma_periods.long))
            .await?;

        let short_term_tx_fee =
            historical_fees::service::calculate_blob_tx_fee(num_blobs, &short_term_sma);

        if self.fee_always_acceptable(short_term_tx_fee) {
            info!("Sending because: short term price {} is deemed always acceptable since it is <= {}", short_term_tx_fee, self.config.fee_thresholds.always_acceptable_fee);
            return Ok(true);
        }

        let long_term_tx_fee =
            historical_fees::service::calculate_blob_tx_fee(num_blobs, &long_term_sma);
        let max_upper_tx_fee = calculate_max_upper_fee(
            &self.config.fee_thresholds,
            long_term_tx_fee,
            num_l2_blocks_behind,
        );

        info!("short_term_tx_fee: {short_term_tx_fee}, long_term_tx_fee: {long_term_tx_fee}, max_upper_tx_fee: {max_upper_tx_fee}");

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
#[cfg(test)]
mod test {
    pub use test_case::test_case;

    use super::Config;
    use crate::{
        historical_fees::{
            port::l1::{testing::PreconfiguredFeeApi, Api, Fees},
            service::{HistoricalFees, SmaPeriods},
        },
        state_committer::{
            fee_algo::{calculate_max_upper_fee, SmaFeeAlgo},
            FeeThresholds, Percentage,
        },
    };

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
        let max_upper_fee = calculate_max_upper_fee(&fee_thresholds, fee, num_l2_blocks_behind);

        assert_eq!(
            max_upper_fee, expected_max_upper_fee,
            "Expected max_upper_fee to be {}, but got {}",
            expected_max_upper_fee, max_upper_fee
        );
    }

    fn generate_fees(sma_periods: SmaPeriods, old_fees: Fees, new_fees: Fees) -> Vec<(u64, Fees)> {
        let older_fees = std::iter::repeat_n(
            old_fees,
            (sma_periods.long.get() - sma_periods.short.get()) as usize,
        );
        let newer_fees = std::iter::repeat_n(new_fees, sma_periods.short.get() as usize);

        older_fees
            .chain(newer_fees)
            .enumerate()
            .map(|(i, f)| (i as u64, f))
            .collect()
    }

    #[test_case(
Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000},
Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000},
6,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    },
},
0, // not behind at all
true;
"Should send because all short-term fees are lower than long-term"
)]
    #[test_case(
Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000},
Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000},
6,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    },
},
0,
false;
"Should not send because all short-term fees are higher than long-term"
)]
    #[test_case(
Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000},
Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000},
6,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        always_acceptable_fee: (21_000 * (5000 + 5000)) + (6 * 131_072 * 5000) + 1,
        max_l2_blocks_behind: 100.try_into().unwrap(),
        ..Default::default()
    }
},
0,
true;
"Should send since short-term fee less than always_acceptable_fee"
)]
    #[test_case(
Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000},
Fees { base_fee_per_gas: 1500, reward: 10000, base_fee_per_blob_gas: 1000},
5,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    }
},
0,
true;
"Should send because short-term base_fee_per_gas is lower"
)]
    #[test_case(
Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000},
Fees { base_fee_per_gas: 2500, reward: 10000, base_fee_per_blob_gas: 1000},
5,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    }
},
0,
false;
"Should not send because short-term base_fee_per_gas is higher"
)]
    #[test_case(
Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1000},
Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 900},
5,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    }
},
0,
true;
"Should send because short-term base_fee_per_blob_gas is lower"
)]
    #[test_case(
Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1000},
Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1100},
5,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    }
},
0,
false;
"Should not send because short-term base_fee_per_blob_gas is higher"
)]
    #[test_case(
Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000},
Fees { base_fee_per_gas: 2000, reward: 9000, base_fee_per_blob_gas: 1000},
5,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    }
},
0,
true;
"Should send because short-term reward is lower"
)]
    #[test_case(
Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000},
Fees { base_fee_per_gas: 2000, reward: 11000, base_fee_per_blob_gas: 1000},
5,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    }
},
0,
false;
"Should not send because short-term reward is higher"
)]
    #[test_case(
// Multiple short-term fees are lower
Fees { base_fee_per_gas: 4000, reward: 8000, base_fee_per_blob_gas: 4000},
Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 3500},
6,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    }
},
0,
true;
"Should send because multiple short-term fees are lower"
)]
    #[test_case(
Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000},
Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000},
6,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    }
},
0,
false;
"Should not send because all fees are identical and no tolerance"
)]
    #[test_case(
// Zero blobs scenario: blob fee differences don't matter
Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000},
Fees { base_fee_per_gas: 2500, reward: 5500, base_fee_per_blob_gas: 5000},
0,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    }
},
0,
true;
"Zero blobs: short-term base_fee_per_gas and reward are lower, send"
)]
    #[test_case(
// Zero blobs but short-term reward is higher
Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000},
Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 5000},
0,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    }
},
0,
false;
"Zero blobs: short-term reward is higher, don't send"
)]
    #[test_case(
Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000},
Fees { base_fee_per_gas: 2000, reward: 6000, base_fee_per_blob_gas: 50_000_000},
0,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        always_acceptable_fee: 0,
        ..Default::default()
    }
},
0,
true;
"Zero blobs: ignore blob fee, short-term base_fee_per_gas is lower, send"
)]
    // Initially not send, but as num_l2_blocks_behind increases, acceptance grows.
    #[test_case(
// Initially short-term fee too high compared to long-term (strict scenario), no send at t=0
Fees { base_fee_per_gas: 6000, reward: 1, base_fee_per_blob_gas: 6000},
Fees { base_fee_per_gas: 7000, reward: 1, base_fee_per_blob_gas: 7000},
1,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        start_discount_percentage: Percentage::try_from(0.20).unwrap(),
        end_premium_percentage: Percentage::try_from(0.20).unwrap(),
        always_acceptable_fee: 0,
    },
},
0,
false;
"Early: short-term expensive, not send"
)]
    #[test_case(
// At max_l2_blocks_behind, send regardless
Fees { base_fee_per_gas: 6000, reward: 1, base_fee_per_blob_gas: 6000},
Fees { base_fee_per_gas: 7000, reward: 1, base_fee_per_blob_gas: 7000},
1,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        start_discount_percentage: 0.20.try_into().unwrap(),
        end_premium_percentage: 0.20.try_into().unwrap(),
        always_acceptable_fee: 0,
    }
},
100,
true;
"Later: after max wait, send regardless"
)]
    #[test_case(
Fees { base_fee_per_gas: 6000, reward: 1, base_fee_per_blob_gas: 6000},
Fees { base_fee_per_gas: 7000, reward: 1, base_fee_per_blob_gas: 7000},
1,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        start_discount_percentage: 0.20.try_into().unwrap(),
        end_premium_percentage: 0.20.try_into().unwrap(),
        always_acceptable_fee: 0,
    },
},
80,
true;
"Mid-wait: increased tolerance allows acceptance"
)]
    #[test_case(
// Short-term fee is huge, but always_acceptable_fee is large, so send immediately
Fees { base_fee_per_gas: 100_000, reward: 1, base_fee_per_blob_gas: 100_000},
Fees { base_fee_per_gas: 2_000_000, reward: 1_000_000, base_fee_per_blob_gas: 20_000_000},
1,
Config {
    sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
    fee_thresholds: FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        start_discount_percentage: 0.20.try_into().unwrap(),
        end_premium_percentage: 0.20.try_into().unwrap(),
        always_acceptable_fee: 2_700_000_000_000
    },
},
0,
true;
"Always acceptable fee triggers immediate send"
)]
    #[tokio::test]
    async fn parameterized_send_or_wait_tests(
        old_fees: Fees,
        new_fees: Fees,
        num_blobs: u32,
        config: Config,
        num_l2_blocks_behind: u32,
        expected_decision: bool,
    ) {
        let fees = generate_fees(config.sma_periods, old_fees, new_fees);
        let api = PreconfiguredFeeApi::new(fees);
        let current_block_height = api.current_height().await.unwrap();
        let historical_fees = HistoricalFees::new(api);

        let sut = SmaFeeAlgo::new(historical_fees, config);

        let should_send = sut
            .fees_acceptable(num_blobs, num_l2_blocks_behind, current_block_height)
            .await
            .unwrap();

        assert_eq!(
    should_send, expected_decision,
    "For num_blobs={num_blobs}, num_l2_blocks_behind={num_l2_blocks_behind}, config={config:?}: Expected decision: {expected_decision}, got: {should_send}",
);
    }
}
