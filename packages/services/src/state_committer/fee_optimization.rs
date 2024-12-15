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
        eprintln!("fee always acceptable: {}", fee_always_acceptable);

        // TODO: segfault test this
        let too_far_behind =
            context.num_l2_blocks_behind >= self.config.fee_thresholds.max_l2_blocks_behind;

        eprintln!("too far behind: {}", too_far_behind);

        if fee_always_acceptable || too_far_behind {
            return true;
        }

        let long_term_tx_fee = Self::calculate_blob_tx_fee(num_blobs, long_term_sma);
        let max_upper_tx_fee = self.calculate_max_upper_fee(long_term_tx_fee, context);

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
    use tokio;

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
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6 },
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6 },
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6 },
            fee_thresholds: Feethresholds {
                always_acceptable_fee: (21_000 * 5000) + (6 * 131_072 * 5000) + 5000 + 1,
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6 },
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6},
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6 },
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6 },
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6},
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6 },
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6},
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6},
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6},
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6},
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6},
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.0,
                end_premium_percentage: 0.0,
                always_acceptable_fee: 0,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6},
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.20,
                end_premium_percentage: 0.20,
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
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6},
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.20,
                end_premium_percentage: 0.20,
                always_acceptable_fee: 0,
            }
        },
        100,
        true;
        "Later: after max wait, send regardless"
    )]
    #[test_case(
        // Partway: at 80 blocks behind, tolerance might have increased enough to accept
        Fees { base_fee_per_gas: 6000, reward: 0, base_fee_per_blob_gas: 6000 },
        Fees { base_fee_per_gas: 7000, reward: 0, base_fee_per_blob_gas: 7000 },
        1,
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6 },
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.20,
                end_premium_percentage: 0.20,
                always_acceptable_fee: 0,
            },
        },
        65,
        true;
        "Mid-wait: increased tolerance allows acceptance"
    )]
    #[test_case(
        // Short-term fee is huge, but always_acceptable_fee is large, so send immediately
        Fees { base_fee_per_gas: 100_000, reward: 0, base_fee_per_blob_gas: 100_000 },
        Fees { base_fee_per_gas: 2_000_000, reward: 1_000_000, base_fee_per_blob_gas: 20_000_000 },
        1,
        Config {
            sma_periods: SmaBlockNumPeriods { short: 2, long: 6 },
            fee_thresholds: Feethresholds {
                max_l2_blocks_behind: 100,
                start_discount_percentage: 0.20,
                end_premium_percentage: 0.20,
                always_acceptable_fee: 26_000_000_000,
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
        num_l2_blocks_behind: u64,
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
                    num_l2_blocks_behind,
                },
            )
            .await;

        assert_eq!(
            should_send, expected_decision,
            "For num_blobs={num_blobs}, num_l2_blocks_behind={num_l2_blocks_behind}, config={config:?}: Expected decision: {expected_decision}, got: {should_send}",
        );
    }
}