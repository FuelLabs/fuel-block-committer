use std::cmp::min;

use crate::state_committer::service::Percentage;

use super::{
    fee_analytics::{FeeAnalytics, FeesProvider},
    port::l1::Fees,
    service::{FeeAlgoConfig, FeeThresholds},
};

pub struct SendOrWaitDecider<P> {
    fee_analytics: FeeAnalytics<P>,
    config: FeeAlgoConfig,
}

impl<P> SendOrWaitDecider<P> {
    pub fn new(fee_provider: P, config: FeeAlgoConfig) -> Self {
        Self {
            fee_analytics: FeeAnalytics::new(fee_provider),
            config,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Context {
    pub num_l2_blocks_behind: u32,
    pub at_l1_height: u64,
}

impl<P: FeesProvider> SendOrWaitDecider<P> {
    // TODO: segfault test that too far behind should work even if we cannot fetch prices due to holes
    // (once that is implemented)
    pub async fn should_send_blob_tx(
        &self,
        num_blobs: u32,
        context: Context,
    ) -> crate::Result<bool> {
        // opted out of validating that num_blobs <= 6, it's not this fn's problem if the caller
        // wants to send more than 6 blobs
        let last_n_blocks = |n: u64| {
            context.at_l1_height.saturating_sub(n.saturating_sub(1))..=context.at_l1_height
        };

        let short_term_sma = self
            .fee_analytics
            .calculate_sma(last_n_blocks(self.config.sma_periods.short))
            .await?;
        eprintln!("short term sma: {:?}", short_term_sma);

        let long_term_sma = self
            .fee_analytics
            .calculate_sma(last_n_blocks(self.config.sma_periods.long))
            .await?;
        eprintln!("long term sma: {:?}", long_term_sma);

        let short_term_tx_fee = Self::calculate_blob_tx_fee(num_blobs, short_term_sma);

        let fee_always_acceptable =
            short_term_tx_fee <= self.config.fee_thresholds.always_acceptable_fee;
        eprintln!("fee always acceptable: {}", fee_always_acceptable);

        let too_far_behind =
            context.num_l2_blocks_behind >= self.config.fee_thresholds.max_l2_blocks_behind.get();

        eprintln!("too far behind: {}", too_far_behind);

        if fee_always_acceptable || too_far_behind {
            return Ok(true);
        }

        let long_term_tx_fee = Self::calculate_blob_tx_fee(num_blobs, long_term_sma);
        let max_upper_tx_fee =
            Self::calculate_max_upper_fee(&self.config.fee_thresholds, long_term_tx_fee, context);

        let long_vs_max_delta_perc =
            ((max_upper_tx_fee as f64 - long_term_tx_fee as f64) / long_term_tx_fee as f64 * 100.)
                .abs();

        let short_vs_max_delta_perc = ((max_upper_tx_fee as f64 - short_term_tx_fee as f64)
            / short_term_tx_fee as f64
            * 100.)
            .abs();

        if long_term_tx_fee <= max_upper_tx_fee {
            eprintln!("The max upper fee({max_upper_tx_fee}) is above the long-term fee({long_term_tx_fee}) by {long_vs_max_delta_perc}%",);
        } else {
            eprintln!("The max upper fee({max_upper_tx_fee}) is below the long-term fee({long_term_tx_fee}) by {long_vs_max_delta_perc}%",);
        }

        if short_term_tx_fee <= max_upper_tx_fee {
            eprintln!("The short term fee({short_term_tx_fee}) is below the max upper fee({max_upper_tx_fee}) by {short_vs_max_delta_perc}%",);
        } else {
            eprintln!("The short term fee({short_term_tx_fee}) is above the max upper fee({max_upper_tx_fee}) by {short_vs_max_delta_perc}%",);
        }

        eprintln!(
            "Short-term fee: {}, Long-term fee: {}, Max upper fee: {}",
            short_term_tx_fee, long_term_tx_fee, max_upper_tx_fee
        );

        Ok(short_term_tx_fee < max_upper_tx_fee)
    }

    fn calculate_max_upper_fee(
        fee_thresholds: &FeeThresholds,
        fee: u128,
        context: Context,
    ) -> u128 {
        let max_blocks_behind = u128::from(fee_thresholds.max_l2_blocks_behind.get());
        let blocks_behind = u128::from(context.num_l2_blocks_behind);

        debug_assert!(
            blocks_behind <= max_blocks_behind,
            "blocks_behind ({}) should not exceed max_blocks_behind ({})",
            blocks_behind,
            max_blocks_behind
        );

        let start_discount_ppm = fee_thresholds.start_discount_percentage.ppm();
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
    use crate::{
        state_committer::{
            fee_analytics::testing::{ConstantFeesProvider, PreconfiguredFeesProvider},
            service::{FeeThresholds, Percentage, SmaPeriods},
        },
        types::NonNegative,
    };

    use test_case::test_case;
    use tokio;

    fn generate_fees(config: FeeAlgoConfig, old_fees: Fees, new_fees: Fees) -> Vec<(u64, Fees)> {
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
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6 },
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
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6 },
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
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6 },
            fee_thresholds: FeeThresholds {
                always_acceptable_fee: (21_000 * 5000) + (6 * 131_072 * 5000) + 5000 + 1,
                max_l2_blocks_behind: 100.try_into().unwrap(),
                ..Default::default()
            }
        },
        0,
        true;
        "Should send since short-term fee < always_acceptable_fee"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 1500, reward: 10000, base_fee_per_blob_gas: 1000 },
        5,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6 },
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
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2500, reward: 10000, base_fee_per_blob_gas: 1000 },
        5,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6},
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
        Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 900 },
        5,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6 },
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
        Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1100 },
        5,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6 },
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
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 9000, base_fee_per_blob_gas: 1000 },
        5,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6},
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
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 11000, base_fee_per_blob_gas: 1000 },
        5,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6 },
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
        Fees { base_fee_per_gas: 4000, reward: 8000, base_fee_per_blob_gas: 4000 },
        Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 3500 },
        6,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6},
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
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6},
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
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 2500, reward: 5500, base_fee_per_blob_gas: 5000 },
        0,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6},
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
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 5000 },
        0,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6},
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
        // Zero blobs don't care about higher short-term base_fee_per_blob_gas
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 2000, reward: 7000, base_fee_per_blob_gas: 50_000_000 },
        0,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6},
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
    Fees { base_fee_per_gas: 6000, reward: 0, base_fee_per_blob_gas: 6000 },
    Fees { base_fee_per_gas: 7000, reward: 0, base_fee_per_blob_gas: 7000 },
        1,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6},
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
        Fees { base_fee_per_gas: 6000, reward: 0, base_fee_per_blob_gas: 6000 },
        Fees { base_fee_per_gas: 7000, reward: 0, base_fee_per_blob_gas: 7000 },
        1,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6},
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
        Fees { base_fee_per_gas: 6000, reward: 0, base_fee_per_blob_gas: 6000 },
        Fees { base_fee_per_gas: 7000, reward: 0, base_fee_per_blob_gas: 7000 },
        1,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6 },
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
        Fees { base_fee_per_gas: 100_000, reward: 0, base_fee_per_blob_gas: 100_000 },
        Fees { base_fee_per_gas: 2_000_000, reward: 1_000_000, base_fee_per_blob_gas: 20_000_000 },
        1,
        FeeAlgoConfig {
            sma_periods: SmaPeriods { short: 2, long: 6 },
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
        config: FeeAlgoConfig,
        num_l2_blocks_behind: u32,
        expected_decision: bool,
    ) {
        let fees = generate_fees(config, old_fees, new_fees);
        let fees_provider = PreconfiguredFeesProvider::new(fees);
        let current_block_height = fees_provider.current_block_height().await.unwrap();

        let sut = SendOrWaitDecider::new(fees_provider, config);

        let should_send = sut
            .should_send_blob_tx(
                num_blobs,
                Context {
                    at_l1_height: current_block_height,
                    num_l2_blocks_behind,
                },
            )
            .await
            .unwrap();

        assert_eq!(
            should_send, expected_decision,
            "For num_blobs={num_blobs}, num_l2_blocks_behind={num_l2_blocks_behind}, config={config:?}: Expected decision: {expected_decision}, got: {should_send}",
        );
    }

    #[test_case(
        // Test Case 1: No blocks behind, no discount or premium
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
        1000,
        Context {
            num_l2_blocks_behind: 0,
            at_l1_height: 0,
        },
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
        Context {
            num_l2_blocks_behind: 50,
            at_l1_height: 0,
        },
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
        Context {
            num_l2_blocks_behind: 50,
            at_l1_height: 0,
        },
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
        Context {
            num_l2_blocks_behind: 50,
            at_l1_height: 0,
        },
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
        Context {
            num_l2_blocks_behind: 99,
            at_l1_height: 0,
        },
        11970;
        "High fee with premium"
    )]
    fn test_calculate_max_upper_fee(
        fee_thresholds: FeeThresholds,
        fee: u128,
        context: Context,
        expected_max_upper_fee: u128,
    ) {
        let max_upper_fee = SendOrWaitDecider::<ConstantFeesProvider>::calculate_max_upper_fee(
            &fee_thresholds,
            fee,
            context,
        );

        assert_eq!(
            max_upper_fee, expected_max_upper_fee,
            "Expected max_upper_fee to be {}, but got {}",
            expected_max_upper_fee, max_upper_fee
        );
    }
}
