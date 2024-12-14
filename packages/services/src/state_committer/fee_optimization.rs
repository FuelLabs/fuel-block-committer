use crate::fee_analytics::port::{l1::FeesProvider, service::FeeAnalytics, Fees};

// TODO: segfault validate percentages
#[derive(Debug, Clone, Copy)]
pub enum ComparisonStrategy {
    /// Short-term fee must be at most (1 - percentage) of the long-term fee.
    /// For example, if percentage = 0.1 (10%), then:
    /// short_term ≤ long_term * 0.9.
    StrictlyLessOrEqualByPercent(f64),

    /// Short-term fee may be more expensive, but not by more than the given percentage.
    /// For example, if percentage = 0.1 (10%), then:
    /// short_term ≤ long_term * 1.1.
    /// Short-term can be cheaper by any amount.
    WithinVicinityOfPriceByPercent(f64),
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub sma_activation_fee_treshold: u128,
    pub short_term_sma_num_blocks: u64,
    pub long_term_sma_num_blocks: u64,
    pub comparison_strategy: ComparisonStrategy,
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

impl<P: FeesProvider> SendOrWaitDecider<P> {
    // TODO: segfault validate blob number
    pub async fn should_send_blob_tx(&self, num_blobs: u32, at_block_height: u64) -> bool {
        let short_term_sma = self
            .fee_analytics
            .calculate_sma(
                at_block_height - self.config.short_term_sma_num_blocks..=at_block_height,
            )
            .await;

        let long_term_sma = self
            .fee_analytics
            .calculate_sma(at_block_height - self.config.long_term_sma_num_blocks..=at_block_height)
            .await;

        let short_term_tx_price = Self::calculate_blob_tx_fee(num_blobs, short_term_sma);

        if short_term_tx_price < self.config.sma_activation_fee_treshold {
            return true;
        }

        let long_term_tx_price = Self::calculate_blob_tx_fee(num_blobs, long_term_sma);

        let percentage = match self.config.comparison_strategy {
            ComparisonStrategy::StrictlyLessOrEqualByPercent(p) => 1.0 - p,
            ComparisonStrategy::WithinVicinityOfPriceByPercent(p) => 1.0 + p,
        };

        // TODO: segfault proper type conversions, probably max(,,) - min(,,) <= delta
        let treshold = (long_term_tx_price as f64 * percentage) as u128;

        // eprintln!(
        //     "Short-term: {}, Long-term: {}, Allowed max: {}, diff: {}",
        //     short_term_tx_price,
        //     long_term_tx_price,
        //     treshold,
        //     short_term_tx_price.saturating_sub(treshold)
        // );

        short_term_tx_price < treshold
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
            (config.long_term_sma_num_blocks - config.short_term_sma_num_blocks) as usize,
        );
        let newer_fees = std::iter::repeat_n(new_fees, config.short_term_sma_num_blocks as usize);

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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 21_000 * 5000 + 6 * 131_072 * 5000 + 5000 + 1,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 21_000 * 5000 + 6 * 131_072 * 5000 + 5000 + 1,
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
            sma_activation_fee_treshold: 0,
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
            sma_activation_fee_treshold: 0,
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
            .should_send_blob_tx(num_blobs, current_block_height)
            .await;

        assert_eq!(
            should_send, expected_decision,
            "For num_blobs={num_blobs}, config={:?}: Expected decision: {}, got: {}",
            config, expected_decision, should_send
        );
    }
}
