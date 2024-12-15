use std::cmp::min;

use crate::fee_analytics::port::{l1::FeesProvider, service::FeeAnalytics, Fees};

#[derive(Debug, Clone, Copy)]
pub struct SmaBlockNumPeriods {
    pub short: u64,
    pub long: u64,
}

// TODO: segfault validate start discount is less than end premium and both are positive
#[derive(Debug, Clone, Copy)]
pub struct Feethresholds {
    // TODO: segfault validate not 0
    pub max_l2_blocks_behind: u64,
    pub start_discount_percentage: f64,
    pub end_premium_percentage: f64,
    pub always_acceptable_fee: u128,
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub sma_periods: SmaBlockNumPeriods,
    pub fee_thresholds: Feethresholds,
}

pub struct SendOrWaitDecider<P> {
    fee_analytics: FeeAnalytics<P>,
    config: Config,
}

impl<P> SendOrWaitDecider<P> {
    pub fn new(fee_analytics: FeeAnalytics<P>, config: Config) -> Self {
        Self {
            fee_analytics,
            config,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Context {
    pub num_l2_blocks_behind: u64,
    pub at_l1_height: u64,
}

impl<P: FeesProvider> SendOrWaitDecider<P> {
    // TODO: segfault validate blob number
    pub async fn should_send_blob_tx(&self, num_blobs: u32, context: Context) -> bool {
        let last_n_blocks = |n: u64| context.at_l1_height.saturating_sub(n)..=context.at_l1_height;

        let short_term_sma = self
            .fee_analytics
            .calculate_sma(last_n_blocks(self.config.sma_periods.short))
            .await;

        let long_term_sma = self
            .fee_analytics
            .calculate_sma(last_n_blocks(self.config.sma_periods.long))
            .await;

        let short_term_tx_fee = Self::calculate_blob_tx_fee(num_blobs, short_term_sma);

        let fee_always_acceptable =
            short_term_tx_fee <= self.config.fee_thresholds.always_acceptable_fee;

        // TODO: segfault test this
        let too_far_behind =
            context.num_l2_blocks_behind >= self.config.fee_thresholds.max_l2_blocks_behind;

        if fee_always_acceptable || too_far_behind {
            return true;
        }

        let long_term_tx_fee = Self::calculate_blob_tx_fee(num_blobs, long_term_sma);
        let max_upper_tx_fee = self.calculate_max_upper_fee(long_term_tx_fee, context);

        short_term_tx_fee < max_upper_tx_fee
    }

    // TODO: segfault test this
    fn calculate_max_upper_fee(&self, fee: u128, context: Context) -> u128 {
        // Define percentages in Parts Per Million (PPM) for precision
        // 1 PPM = 0.0001%
        const PPM: u128 = 1_000_000;
        let start_discount_ppm =
            (self.config.fee_thresholds.start_discount_percentage * PPM as f64) as u128;
        let end_premium_ppm =
            (self.config.fee_thresholds.end_premium_percentage * PPM as f64) as u128;

        let max_blocks_behind = self.config.fee_thresholds.max_l2_blocks_behind as u128;

        let blocks_behind = context.num_l2_blocks_behind;

        // TODO: segfault rename possibly
        let ratio_ppm = (blocks_behind as u128 * PPM) / max_blocks_behind;

        let initial_multiplier = PPM.saturating_sub(start_discount_ppm);

        let effect_of_being_late = (start_discount_ppm + end_premium_ppm)
            .saturating_mul(ratio_ppm)
            .saturating_div(PPM);

        let multiplier_ppm = initial_multiplier.saturating_add(effect_of_being_late);

        // TODO: segfault, for now just in case, but this should never happen
        let multiplier_ppm = min(PPM + end_premium_ppm, multiplier_ppm);

        fee.saturating_mul(multiplier_ppm).saturating_div(PPM)
    }

    // TODO: Segfault maybe dont leak so much eth abstractions
    fn calculate_blob_tx_fee(num_blobs: u32, fees: Fees) -> u128 {
        const DATA_GAS_PER_BLOB: u128 = 131_072u128;
        const INTRINSIC_GAS: u128 = 21_000u128;

        let base_fee = INTRINSIC_GAS * fees.base_fee_per_gas;
        let blob_fee = fees.base_fee_per_blob_gas * num_blobs as u128 * DATA_GAS_PER_BLOB;

        base_fee + blob_fee + fees.reward
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fee_analytics::port::{l1::testing::TestFeesProvider, Fees};
    use test_case::test_case;

    // Function to generate historical fees data
    fn generate_fees(config: Config, old_fees: Fees, new_fees: Fees) -> Vec<(u64, Fees)> {
        let older_fees = std::iter::repeat_n(
            old_fees,
            (config.sma_periods.long - config.sma_periods.short) as usize,
        );
        let newer_fees = std::iter::repeat_n(new_fees, config.sma_periods.short as usize);

        older_fees
            .chain(newer_fees)
            .enumerate()
            .map(|(i, f)| (i as u64, f))
            .collect()
    }

    #[test_case(
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        6,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        true;
        "Should send because all short-term fees are lower than long-term"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        false;
        "Should not send because all short-term fees are higher than long-term"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        Config {
            sma_activation_fee_threshold: 21_000 * 5000 + 6 * 131_072 * 5000 + 5000 + 1,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        true;
        "Should send since we're below the activation fee threshold, even if all short-term fees are higher than long-term"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 1500, reward: 10000, base_fee_per_blob_gas: 1000 },
        5,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        true;
        "Should send because short-term base_fee_per_gas is lower"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2500, reward: 10000, base_fee_per_blob_gas: 1000 },
        5,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        false;
        "Should not send because short-term base_fee_per_gas is higher"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 900 },
        5,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        true;
        "Should send because short-term base_fee_per_blob_gas is lower"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1100 },
        5,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        false;
        "Should not send because short-term base_fee_per_blob_gas is higher"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 9000, base_fee_per_blob_gas: 1000 },
        5,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        true;
        "Should send because short-term reward is lower"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 11000, base_fee_per_blob_gas: 1000 },
        5,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        false;
        "Should not send because short-term reward is higher"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 4000, reward: 8000, base_fee_per_blob_gas: 4000 },
        Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 3500 },
        6,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        true;
        "Should send because multiple short-term fees are lower"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        false;
        "Should not send because all fees are identical"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 2500, reward: 5500, base_fee_per_blob_gas: 5000 },
        0,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        true;
        "Zero blobs but short-term base_fee_per_gas and reward are lower"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 5000 },
        0,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        false;
        "Zero blobs but short-term reward is higher"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 2000, reward: 7000, base_fee_per_blob_gas: 50_000_000 },
        0,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        true;
        "Zero blobs dont care about higher short-term base_fee_per_blob_gas"
    )]
    // New Tests (Introducing other Comparison Strategies)
    #[test_case(
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        6,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.1)
        },
        true;
        "Should send (cheaper) with StrictlyLessOrEqualByPercent(10%)"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            // Strictly less or equal by 0% means must be cheaper or equal, which it's not
            comparison_strategy: ComparisonStrategy::StrictlyLessOrEqualByPercent(0.0)
        },
        false;
        "Should not send (more expensive) with StrictlyLessOrEqualByPercent(0%)"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        Config {
            // Below threshold means we send anyway
            sma_activation_fee_threshold: 21_000 * 5000 + 6 * 131_072 * 5000 + 5000 + 1,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::WithinVicinityOfPriceByPercent(0.1)
        },
        true;
        "Below activation threshold, send anyway for WithinVicinityOfPriceByPercent(10%)"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        Fees { base_fee_per_gas: 3200, reward: 5000, base_fee_per_blob_gas: 3200 },
        6,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::WithinVicinityOfPriceByPercent(0.1)
        },
        true;
        "Within vicinity of price by 10%: short_term slightly more expensive but allowed"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        Config {
            sma_activation_fee_threshold: 0,
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
            comparison_strategy: ComparisonStrategy::WithinVicinityOfPriceByPercent(0.0)
        },
        false;
        "Within vicinity with 0% means must not exceed long-term, not sending"
    )]
    #[tokio::test]
    async fn parameterized_send_or_wait_tests(
        old_fees: Fees,
        new_fees: Fees,
        num_blobs: u32,
        config: Config,
        expected_decision: bool,
    ) {
        let fees = generate_fees(config, old_fees, new_fees);
        let fees_provider = TestFeesProvider::new(fees);
        let current_block_height = fees_provider.current_block_height().await;
        let analytics_service = FeeAnalytics::new(fees_provider);

        let sut = SendOrWaitDecider::new(analytics_service, config);

        let should_send = sut
            .should_send_blob_tx(
                num_blobs,
                Context {
                    at_l1_height: current_block_height,
                    num_l2_blocks_behind: 0,
                },
            )
            .await;

        assert_eq!(
            should_send, expected_decision,
            "For num_blobs={num_blobs}, config={:?}: Expected decision: {}, got: {}",
            config, expected_decision, should_send
        );
    }
}
