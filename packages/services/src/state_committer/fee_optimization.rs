use crate::historical_fees::port::{l1::FeesProvider, service::HistoricalFeesProvider, Fees};

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub short_term_sma_num_blocks: u64,
    pub long_term_sma_num_blocks: u64,
}

pub struct SendOrWaitDecider<P> {
    price_service: HistoricalFeesProvider<P>,
    config: Config,
}

impl<P> SendOrWaitDecider<P> {
    pub fn new(price_service: HistoricalFeesProvider<P>, config: Config) -> Self {
        Self {
            price_service,
            config,
        }
    }
}

impl<P: FeesProvider> SendOrWaitDecider<P> {
    // TODO: segfault validate blob number
    pub async fn should_send_blob_tx(&self, num_blobs: u32) -> bool {
        let short_term_sma = self
            .price_service
            .calculate_sma(self.config.short_term_sma_num_blocks)
            .await;

        let long_term_sma = self
            .price_service
            .calculate_sma(self.config.long_term_sma_num_blocks)
            .await;

        let short_term_tx_price = Self::calculate_blob_tx_fee(num_blobs, short_term_sma);
        let long_term_tx_price = Self::calculate_blob_tx_fee(num_blobs, long_term_sma);

        short_term_tx_price < long_term_tx_price
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
    use crate::historical_fees::port::{l1::testing::TestFeesProvider, Fees};
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

    #[tokio::test]
    #[test_case(
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 }, // Old fees
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 }, // New fees
        10,                                                                    // num_blobs
        true; 
        "Should send because all short-term fees are lower than long-term"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 }, // Old fees
        Fees { base_fee_per_gas: 1500, reward: 10000, base_fee_per_blob_gas: 1000 }, // New fees
        5,
        true;
        "Should send because short-term base_fee_per_gas is lower"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 }, // Old fees
        Fees { base_fee_per_gas: 2500, reward: 10000, base_fee_per_blob_gas: 1000 }, // New fees
        5,
        false;
        "Should not send because short-term base_fee_per_gas is higher"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 }, // Old fees
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 900 },  // New fees
        5,
        true;
        "Should send because short-term base_fee_per_blob_gas is lower"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 }, // Old fees
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1100 }, // New fees
        5,
        false;
        "Should not send because short-term base_fee_per_blob_gas is higher"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 }, // Old fees
        Fees { base_fee_per_gas: 2000, reward: 9000, base_fee_per_blob_gas: 1000 },  // New fees
        5,
        true;
        "Should send because short-term reward is lower"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 }, // Old fees
        Fees { base_fee_per_gas: 2000, reward: 11000, base_fee_per_blob_gas: 1000 }, // New fees
        5,
        false;
        "Should not send because short-term reward is higher"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 4000, reward: 8000, base_fee_per_blob_gas: 4000 }, // Old fees
        Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 3500 }, // New fees
        10,
        true;
        "Should send because multiple short-term fees are lower"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 }, // Old fees
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 }, // New fees
        10,
        false;
        "Should not send because all fees are identical"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 }, // Old fees
        Fees { base_fee_per_gas: 2500, reward: 5500, base_fee_per_blob_gas: 5000 }, // New fees
        0,
        true;
        "Zero blobs but short-term base_fee_per_gas and reward are lower"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 }, // Old fees
        Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 5000 }, // New fees
        0,
        false;
        "Zero blobs but short-term reward is higher"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 }, // Old fees
        Fees { base_fee_per_gas: 2000, reward: 7000, base_fee_per_blob_gas: 50_000_000 }, // New fees
        0,
        true;
        "Zero blobs dont care about higher short-term base_fee_per_blob_gas"
    )]
    async fn parameterized_send_or_wait_tests(
        old_fees: Fees,
        new_fees: Fees,
        num_blobs: u32,
        expected_decision: bool,
    ) {
        // Given
        let config = Config {
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
        };

        let fees = generate_fees(config, old_fees, new_fees);
        let fees_provider = TestFeesProvider::new(fees);
        let price_service = HistoricalFeesProvider::new(fees_provider);

        let sut = SendOrWaitDecider::new(price_service, config);

        // When
        let should_send = sut.should_send_blob_tx(num_blobs).await;

        // Then
        assert_eq!(
            should_send, expected_decision,
            "For num_blobs={num_blobs}: Expected decision: {expected_decision}, got: {should_send}",
        );
    }
}
