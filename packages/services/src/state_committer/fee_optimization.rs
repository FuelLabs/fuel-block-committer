use std::{ops::RangeInclusive, time::Duration};

use crate::historical_fees::port::{l1::FeesProvider, service::HistoricalFeesProvider, Fees};
use alloy::eips::eip4844::DATA_GAS_PER_BLOB;
use nonempty::NonEmpty;
use rayon::range_inclusive;

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
        eprintln!(
            "Short term: {:?}, Long term: {:?}, Short term tx price: {}, Long term tx price: {}",
            short_term_sma, long_term_sma, short_term_tx_price, long_term_tx_price
        );

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
    use std::{
        collections::{BTreeMap, BTreeSet, HashMap},
        time::Duration,
    };

    use crate::historical_fees::port::{
        l1::testing::{incrementing_fees, TestFeesProvider},
        BlockFees, Fees,
    };

    use crate::types::CollectNonEmpty;

    use super::*;

    #[tokio::test]
    async fn should_send_if_shortterm_prices_lower_than_longterm_ones() {
        // given
        let config = Config {
            short_term_sma_num_blocks: 2,
            long_term_sma_num_blocks: 6,
        };
        let fees = [
            (
                1,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                2,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                3,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                4,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                5,
                Fees {
                    base_fee_per_gas: 20,
                    reward: 20,
                    base_fee_per_blob_gas: 20,
                },
            ),
            (
                6,
                Fees {
                    base_fee_per_gas: 20,
                    reward: 20,
                    base_fee_per_blob_gas: 20,
                },
            ),
        ];

        let fees_provider = TestFeesProvider::new(fees);
        let price_service = HistoricalFeesProvider::new(fees_provider);

        let short_sma = price_service.calculate_sma(2).await;
        let long_sma = price_service.calculate_sma(6).await;
        assert_eq!(
            long_sma,
            Fees {
                base_fee_per_gas: 40,
                reward: 40,
                base_fee_per_blob_gas: 40
            }
        );
        assert_eq!(
            short_sma,
            Fees {
                base_fee_per_gas: 20,
                reward: 20,
                base_fee_per_blob_gas: 20
            }
        );

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
        let fees = [
            (
                1,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                2,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                3,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                4,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                5,
                Fees {
                    base_fee_per_gas: 10000,
                    reward: 20,
                    base_fee_per_blob_gas: 20,
                },
            ),
            (
                6,
                Fees {
                    base_fee_per_gas: 10000,
                    reward: 20,
                    base_fee_per_blob_gas: 20,
                },
            ),
        ];

        let fees_provider = TestFeesProvider::new(fees);
        let price_service = HistoricalFeesProvider::new(fees_provider);

        let short_sma = price_service.calculate_sma(2).await;
        let long_sma = price_service.calculate_sma(6).await;
        assert_eq!(
            long_sma,
            Fees {
                base_fee_per_gas: 3366,
                reward: 40,
                base_fee_per_blob_gas: 40
            }
        );
        assert_eq!(
            short_sma,
            Fees {
                base_fee_per_gas: 10000,
                reward: 20,
                base_fee_per_blob_gas: 20
            }
        );

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
        let fees = [
            (
                1,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                2,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                3,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                4,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                5,
                Fees {
                    base_fee_per_gas: 20,
                    reward: 20,
                    base_fee_per_blob_gas: 1000,
                },
            ),
            (
                6,
                Fees {
                    base_fee_per_gas: 20,
                    reward: 20,
                    base_fee_per_blob_gas: 1000,
                },
            ),
        ];

        let fees_provider = TestFeesProvider::new(fees);
        let price_service = HistoricalFeesProvider::new(fees_provider);

        let short_sma = price_service.calculate_sma(2).await;
        let long_sma = price_service.calculate_sma(6).await;
        assert_eq!(
            long_sma,
            Fees {
                base_fee_per_gas: 40,
                reward: 40,
                base_fee_per_blob_gas: 366
            }
        );
        assert_eq!(
            short_sma,
            Fees {
                base_fee_per_gas: 20,
                reward: 20,
                base_fee_per_blob_gas: 1000
            }
        );

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
        let fees = [
            (
                1,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                2,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                3,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                4,
                Fees {
                    base_fee_per_gas: 50,
                    reward: 50,
                    base_fee_per_blob_gas: 50,
                },
            ),
            (
                5,
                Fees {
                    base_fee_per_gas: 20,
                    reward: 100_000_000,
                    base_fee_per_blob_gas: 20,
                },
            ),
            (
                6,
                Fees {
                    base_fee_per_gas: 20,
                    reward: 100_000_000,
                    base_fee_per_blob_gas: 20,
                },
            ),
        ];

        let fees_provider = TestFeesProvider::new(fees);
        let price_service = HistoricalFeesProvider::new(fees_provider);

        let short_sma = price_service.calculate_sma(2).await;
        let long_sma = price_service.calculate_sma(6).await;
        assert_eq!(
            long_sma,
            Fees {
                base_fee_per_gas: 40,
                reward: 33333366,
                base_fee_per_blob_gas: 40,
            }
        );
        assert_eq!(
            short_sma,
            Fees {
                base_fee_per_gas: 20,
                reward: 100_000_000,
                base_fee_per_blob_gas: 20
            }
        );

        let sut = SendOrWaitDecider::new(price_service, config);
        let num_blobs = 6;

        // when
        let decision = sut.should_send_blob_tx(num_blobs).await;

        // then
        assert!(!decision, "Should not have sent");
    }
}
