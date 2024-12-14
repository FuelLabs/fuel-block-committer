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

    use crate::historical_fees::port::{l1::testing::TestFeesProvider, Fees};

    use super::*;

    fn constant_fees(num_blocks: u64, fees: Fees) -> Vec<(u64, Fees)> {
        (0..=num_blocks).map(|height| (height, fees)).collect()
    }

    fn update_last_n_fees(fees: &mut [(u64, Fees)], num_latest: u64, updated_fees: Fees) {
        fees.iter_mut()
            .rev()
            .take(num_latest as usize)
            .for_each(|(_, f)| *f = updated_fees);
    }

    #[tokio::test]
    async fn should_send_if_shortterm_prices_lower_than_longterm_ones() {
        // given
        let config = Config {
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
        };

        let mut fees = constant_fees(
            config.long_term_sma_num_blocks,
            Fees {
                base_fee_per_gas: 50,
                reward: 50,
                base_fee_per_blob_gas: 50,
            },
        );

        update_last_n_fees(
            &mut fees,
            config.short_term_sma_num_blocks,
            Fees {
                base_fee_per_gas: 20,
                reward: 20,
                base_fee_per_blob_gas: 20,
            },
        );

        let fees_provider = TestFeesProvider::new(fees);
        let price_service = HistoricalFeesProvider::new(fees_provider);

        let sut = SendOrWaitDecider::new(price_service, config);
        let num_blobs = 6;

        // when
        let decision = sut.should_send_blob_tx(num_blobs).await;

        // then
        assert!(decision, "Should have sent");
    }

    #[tokio::test]
    async fn wont_send_because_normal_gas_too_expensive() {
        // given
        let config = Config {
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
        };

        let mut fees = constant_fees(
            config.long_term_sma_num_blocks,
            Fees {
                base_fee_per_gas: 50,
                reward: 50,
                base_fee_per_blob_gas: 50,
            },
        );

        update_last_n_fees(
            &mut fees,
            config.short_term_sma_num_blocks,
            Fees {
                base_fee_per_gas: 10000,
                reward: 20,
                base_fee_per_blob_gas: 20,
            },
        );

        let fees_provider = TestFeesProvider::new(fees);
        let price_service = HistoricalFeesProvider::new(fees_provider);

        let sut = SendOrWaitDecider::new(price_service, config);
        let num_blobs = 6;

        // when
        let decision = sut.should_send_blob_tx(num_blobs).await;

        // then
        assert!(!decision, "Should not have sent");
    }

    #[tokio::test]
    async fn wont_send_because_blob_gas_too_expensive() {
        // given
        let config = Config {
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
        };

        let mut fees = constant_fees(
            config.long_term_sma_num_blocks,
            Fees {
                base_fee_per_gas: 50,
                reward: 50,
                base_fee_per_blob_gas: 50,
            },
        );

        update_last_n_fees(
            &mut fees,
            config.short_term_sma_num_blocks,
            Fees {
                base_fee_per_gas: 20,
                reward: 20,
                base_fee_per_blob_gas: 1000,
            },
        );

        let fees_provider = TestFeesProvider::new(fees);
        let price_service = HistoricalFeesProvider::new(fees_provider);

        let sut = SendOrWaitDecider::new(price_service, config);
        let num_blobs = 6;

        // when
        let decision = sut.should_send_blob_tx(num_blobs).await;

        // then
        assert!(!decision, "Should not have sent");
    }

    #[tokio::test]
    async fn wont_send_because_rewards_too_expensive() {
        // given
        let config = Config {
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
        };

        let mut fees = constant_fees(
            config.long_term_sma_num_blocks,
            Fees {
                base_fee_per_gas: 50,
                reward: 50,
                base_fee_per_blob_gas: 50,
            },
        );

        update_last_n_fees(
            &mut fees,
            config.short_term_sma_num_blocks,
            Fees {
                base_fee_per_gas: 20,
                reward: 100_000_000,
                base_fee_per_blob_gas: 20,
            },
        );

        let fees_provider = TestFeesProvider::new(fees);
        let price_service = HistoricalFeesProvider::new(fees_provider);

        let sut = SendOrWaitDecider::new(price_service, config);
        let num_blobs = 6;

        // when
        let decision = sut.should_send_blob_tx(num_blobs).await;

        // then
        assert!(!decision, "Should not have sent");
    }
}
