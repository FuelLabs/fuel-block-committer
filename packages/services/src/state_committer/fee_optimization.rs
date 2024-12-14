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
        Fees { base_fee_per_gas: 50, reward: 50, base_fee_per_blob_gas: 50 },
        Fees { base_fee_per_gas: 20, reward: 20, base_fee_per_blob_gas: 20 },
        true; "Should send because short-term prices lower than long-term"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 50, reward: 50, base_fee_per_blob_gas: 50 },
        Fees { base_fee_per_gas: 10_000, reward: 20, base_fee_per_blob_gas: 20 },
        false; "Wont send because normal gas too expensive"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 50, reward: 50, base_fee_per_blob_gas: 50 },
        Fees { base_fee_per_gas: 20, reward: 20, base_fee_per_blob_gas: 1000 },
        false; "Wont send because blob gas too expensive"
    )]
    #[test_case(
        Fees { base_fee_per_gas: 50, reward: 50, base_fee_per_blob_gas: 50 },
        Fees { base_fee_per_gas: 20, reward: 100_000_000, base_fee_per_blob_gas: 20 },
        false; "Wont send because rewards too expensive"
    )]
    async fn parameterized_send_or_wait_tests(
        old_fees: Fees,
        new_fees: Fees,
        expected_decision: bool,
    ) {
        // given
        let config = Config {
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
        };

        let fees = generate_fees(config, old_fees, new_fees);
        let fees_provider = TestFeesProvider::new(fees);
        let price_service = HistoricalFeesProvider::new(fees_provider);

        let sut = SendOrWaitDecider::new(price_service, config);
        let num_blobs = 6;

        // when
        let should_send = sut.should_send_blob_tx(num_blobs).await;

        // then
        assert_eq!(
            should_send, expected_decision,
            "Expected decision: {expected_decision}, got: {should_send}",
        );
    }
}
