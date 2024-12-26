use std::{num::NonZeroU128, ops::RangeInclusive};

use super::port::l1::{Api, BlockFees, Fees, SequentialBlockFees};
use crate::Error;

// TODO: segfault, move this higher because it is used by both the state committer and the fee
// tracker
#[derive(Debug, Clone)]
pub struct FeeAnalytics<P> {
    fees_provider: P,
}

impl<P> FeeAnalytics<P> {
    pub fn new(fees_provider: P) -> Self {
        Self { fees_provider }
    }
}

impl<P: Api> FeeAnalytics<P> {
    pub async fn calculate_sma(&self, block_range: RangeInclusive<u64>) -> crate::Result<Fees> {
        let fees = self.fees_provider.fees(block_range.clone()).await?;

        let received_height_range = fees.height_range();
        if received_height_range != block_range {
            return Err(Error::from(format!(
                "fees received from the adapter({received_height_range:?}) don't cover the requested range ({block_range:?})"
            )));
        }

        Ok(Self::mean(fees))
    }

    pub async fn latest_fees(&self) -> crate::Result<BlockFees> {
        let height = self.fees_provider.current_height().await?;

        let fee = self
            .fees_provider
            .fees(height..=height)
            .await?
            .into_iter()
            .next()
            .expect("sequential fees guaranteed not empty");

        Ok(fee)
    }

    fn mean(fees: SequentialBlockFees) -> Fees {
        let count = fees.len() as u128;

        let total = fees
            .into_iter()
            .map(|bf| bf.fees)
            .fold(Fees::default(), |acc, f| {
                let base_fee_per_gas = acc
                    .base_fee_per_gas
                    .saturating_add(f.base_fee_per_gas.get());
                let reward = acc.reward.saturating_add(f.reward.get());
                let base_fee_per_blob_gas = acc
                    .base_fee_per_blob_gas
                    .saturating_add(f.base_fee_per_blob_gas.get());

                Fees {
                    base_fee_per_gas,
                    reward,
                    base_fee_per_blob_gas,
                }
            });

        let divide_by_count = |value: NonZeroU128| {
            let minimum_fee = NonZeroU128::try_from(1).unwrap();
            value
                .get()
                .saturating_div(count)
                .try_into()
                .unwrap_or(minimum_fee)
        };

        Fees {
            base_fee_per_gas: divide_by_count(total.base_fee_per_gas),
            reward: divide_by_count(total.reward),
            base_fee_per_blob_gas: divide_by_count(total.base_fee_per_blob_gas),
        }
    }
}

pub fn calculate_blob_tx_fee(num_blobs: u32, fees: &Fees) -> u128 {
    const DATA_GAS_PER_BLOB: u128 = 131_072u128;
    const INTRINSIC_GAS: u128 = 21_000u128;

    let base_fee = INTRINSIC_GAS.saturating_mul(fees.base_fee_per_gas.get());
    let blob_fee = fees
        .base_fee_per_blob_gas
        .get()
        .saturating_mul(u128::from(num_blobs))
        .saturating_mul(DATA_GAS_PER_BLOB);
    let reward_fee = fees.reward.get().saturating_mul(INTRINSIC_GAS);

    base_fee.saturating_add(blob_fee).saturating_add(reward_fee)
}

#[cfg(test)]
mod tests {
    use itertools::Itertools;

    use super::*;
    use crate::fee_metrics_updater::port::l1::{testing, BlockFees};

    #[test]
    fn can_create_valid_sequential_fees() {
        // Given
        let block_fees = vec![
            BlockFees {
                height: 1,
                fees: Fees {
                    base_fee_per_gas: 100.try_into().unwrap(),
                    reward: 50.try_into().unwrap(),
                    base_fee_per_blob_gas: 10.try_into().unwrap(),
                },
            },
            BlockFees {
                height: 2,
                fees: Fees {
                    base_fee_per_gas: 110.try_into().unwrap(),
                    reward: 55.try_into().unwrap(),
                    base_fee_per_blob_gas: 15.try_into().unwrap(),
                },
            },
        ];

        // When
        let result = SequentialBlockFees::try_from(block_fees.clone());

        // Then
        assert!(
            result.is_ok(),
            "Expected SequentialBlockFees creation to succeed"
        );
        let sequential_fees = result.unwrap();
        assert_eq!(sequential_fees.len(), block_fees.len());
    }

    #[test]
    fn sequential_fees_cannot_be_empty() {
        // Given
        let block_fees: Vec<BlockFees> = vec![];

        // When
        let result = SequentialBlockFees::try_from(block_fees);

        // Then
        assert!(
            result.is_err(),
            "Expected SequentialBlockFees creation to fail for empty input"
        );
        assert_eq!(
            result.unwrap_err().to_string(),
            "InvalidSequence(\"Input cannot be empty\")"
        );
    }

    #[test]
    fn fees_must_be_sequential() {
        // Given
        let block_fees = vec![
            BlockFees {
                height: 1,
                fees: Fees {
                    base_fee_per_gas: 100.try_into().unwrap(),
                    reward: 50.try_into().unwrap(),
                    base_fee_per_blob_gas: 10.try_into().unwrap(),
                },
            },
            BlockFees {
                height: 3, // Non-sequential height
                fees: Fees {
                    base_fee_per_gas: 110.try_into().unwrap(),
                    reward: 55.try_into().unwrap(),
                    base_fee_per_blob_gas: 15.try_into().unwrap(),
                },
            },
        ];

        // When
        let result = SequentialBlockFees::try_from(block_fees);

        // Then
        assert!(
            result.is_err(),
            "Expected SequentialBlockFees creation to fail for non-sequential heights"
        );
        assert_eq!(
            result.unwrap_err().to_string(),
            "InvalidSequence(\"blocks are not sequential by height: [1, 3]\")"
        );
    }

    #[test]
    fn produced_iterator_gives_correct_values() {
        // Given
        // notice the heights are out of order so that we validate that the returned sequence is in
        // order
        let block_fees = vec![
            BlockFees {
                height: 2,
                fees: Fees {
                    base_fee_per_gas: 110.try_into().unwrap(),
                    reward: 55.try_into().unwrap(),
                    base_fee_per_blob_gas: 15.try_into().unwrap(),
                },
            },
            BlockFees {
                height: 1,
                fees: Fees {
                    base_fee_per_gas: 100.try_into().unwrap(),
                    reward: 50.try_into().unwrap(),
                    base_fee_per_blob_gas: 10.try_into().unwrap(),
                },
            },
        ];
        let sequential_fees = SequentialBlockFees::try_from(block_fees.clone()).unwrap();

        // When
        let iterated_fees: Vec<BlockFees> = sequential_fees.into_iter().collect();

        // Then
        let expectation = block_fees
            .into_iter()
            .sorted_by_key(|b| b.height)
            .collect_vec();
        assert_eq!(
            iterated_fees, expectation,
            "Expected iterator to yield the same block fees"
        );
    }

    #[tokio::test]
    async fn calculates_sma_correctly_for_last_1_block() {
        // given
        let fees_provider = testing::PreconfiguredFeeApi::new(testing::incrementing_fees(5));
        let fee_analytics = FeeAnalytics::new(fees_provider);

        // when
        let sma = fee_analytics.calculate_sma(4..=4).await.unwrap();

        // then
        assert_eq!(sma.base_fee_per_gas, 6.try_into().unwrap());
        assert_eq!(sma.reward, 6.try_into().unwrap());
        assert_eq!(sma.base_fee_per_blob_gas, 6.try_into().unwrap());
    }

    #[tokio::test]
    async fn calculates_sma_correctly_for_last_5_blocks() {
        // given
        let fees_provider = testing::PreconfiguredFeeApi::new(testing::incrementing_fees(5));
        let fee_analytics = FeeAnalytics::new(fees_provider);

        // when
        let sma = fee_analytics.calculate_sma(0..=4).await.unwrap();

        // then
        let mean = ((5 + 4 + 3 + 2 + 1) / 5).try_into().unwrap();
        assert_eq!(sma.base_fee_per_gas, mean);
        assert_eq!(sma.reward, mean);
        assert_eq!(sma.base_fee_per_blob_gas, mean);
    }

    #[tokio::test]
    async fn errors_out_if_returned_fees_are_not_complete() {
        // given
        let mut fees = testing::incrementing_fees(5);
        fees.remove(&4);
        let fees_provider = testing::PreconfiguredFeeApi::new(fees);
        let fee_analytics = FeeAnalytics::new(fees_provider);

        // when
        let err = fee_analytics
            .calculate_sma(0..=4)
            .await
            .expect_err("should have failed because returned fees are not complete");

        // then
        assert_eq!(
            err.to_string(),
            "fees received from the adapter(0..=3) don't cover the requested range (0..=4)"
        );
    }

    #[tokio::test]
    async fn latest_fees_on_fee_analytics() {
        // given
        let fees_map = testing::incrementing_fees(5);
        let fees_provider = testing::PreconfiguredFeeApi::new(fees_map.clone());
        let fee_analytics = FeeAnalytics::new(fees_provider);
        let height = 4;

        // when
        let fee = fee_analytics.latest_fees().await.unwrap();

        // then
        let expected_fee = BlockFees {
            height,
            fees: Fees {
                base_fee_per_gas: 5.try_into().unwrap(),
                reward: 5.try_into().unwrap(),
                base_fee_per_blob_gas: 5.try_into().unwrap(),
            },
        };
        assert_eq!(
            fee, expected_fee,
            "Fee at height {height} should be {expected_fee:?}"
        );
    }

    #[tokio::test]
    async fn mean_is_at_least_one_when_totals_are_zero() {
        // given
        let block_fees = vec![
            BlockFees {
                height: 1,
                fees: Fees {
                    base_fee_per_gas: 1.try_into().unwrap(),
                    reward: 1.try_into().unwrap(),
                    base_fee_per_blob_gas: 1.try_into().unwrap(),
                },
            },
            BlockFees {
                height: 2,
                fees: Fees {
                    base_fee_per_gas: 1.try_into().unwrap(),
                    reward: 1.try_into().unwrap(),
                    base_fee_per_blob_gas: 1.try_into().unwrap(),
                },
            },
        ];
        let sequential_fees = SequentialBlockFees::try_from(block_fees).unwrap();
        let mean = FeeAnalytics::<testing::PreconfiguredFeeApi>::mean(sequential_fees.clone());

        // then
        assert_eq!(
            mean.base_fee_per_gas,
            1.try_into().unwrap(),
            "base_fee_per_gas should be set to 1 when total is 0"
        );
        assert_eq!(
            mean.reward,
            1.try_into().unwrap(),
            "reward should be set to 1 when total is 0"
        );
        assert_eq!(
            mean.base_fee_per_blob_gas,
            1.try_into().unwrap(),
            "base_fee_per_blob_gas should be set to 1 when total is 0"
        );
    }

    // fn calculate_tx_fee(fees: &Fees) -> u128 {
    //     21_000 * fees.base_fee_per_gas + fees.reward + 6 * fees.base_fee_per_blob_gas * 131_072
    // }
    //
    // fn save_tx_fees(tx_fees: &[(u64, u128)], path: &str) {
    //     let mut csv_writer =
    //         csv::Writer::from_path(PathBuf::from("/home/segfault_magnet/grafovi/").join(path))
    //             .unwrap();
    //     csv_writer
    //         .write_record(["height", "tx_fee"].iter())
    //         .unwrap();
    //     for (height, fee) in tx_fees {
    //         csv_writer
    //             .write_record([height.to_string(), fee.to_string()])
    //             .unwrap();
    //     }
    //     csv_writer.flush().unwrap();
    // }

    // #[tokio::test]
    // async fn something() {
    //     let client = make_pub_eth_client().await;
    //     use services::fee_analytics::port::l1::FeesProvider;
    //
    //     let current_block_height = 21408300;
    //     let starting_block_height = current_block_height - 48 * 3600 / 12;
    //     let data = client
    //         .fees(starting_block_height..=current_block_height)
    //         .await
    //         .into_iter()
    //         .collect::<Vec<_>>();
    //
    //     let fee_lookup = data
    //         .iter()
    //         .map(|b| (b.height, b.fees))
    //         .collect::<HashMap<_, _>>();
    //
    //     let short_sma = 25u64;
    //     let long_sma = 900;
    //
    //     let current_tx_fees = data
    //         .iter()
    //         .map(|b| (b.height, calculate_tx_fee(&b.fees)))
    //         .collect::<Vec<_>>();
    //
    //     save_tx_fees(&current_tx_fees, "current_fees.csv");
    //
    //     let local_client = TestFeesProvider::new(data.clone().into_iter().map(|e| (e.height, e.fees)));
    //     let fee_analytics = FeeAnalytics::new(local_client.clone());
    //
    //     let mut short_sma_tx_fees = vec![];
    //     for height in (starting_block_height..=current_block_height).skip(short_sma as usize) {
    //         let fees = fee_analytics
    //             .calculate_sma(height - short_sma..=height)
    //             .await;
    //
    //         let tx_fee = calculate_tx_fee(&fees);
    //
    //         short_sma_tx_fees.push((height, tx_fee));
    //     }
    //     save_tx_fees(&short_sma_tx_fees, "short_sma_fees.csv");
    //
    //     let decider = SendOrWaitDecider::new(
    //         FeeAnalytics::new(local_client.clone()),
    //         services::state_committer::fee_optimization::Config {
    //             sma_periods: services::state_committer::fee_optimization::SmaBlockNumPeriods {
    //                 short: short_sma,
    //                 long: long_sma,
    //             },
    //             fee_thresholds: Feethresholds {
    //                 max_l2_blocks_behind: 43200 * 3,
    //                 start_discount_percentage: 0.2,
    //                 end_premium_percentage: 0.2,
    //                 always_acceptable_fee: 1000000000000000u128,
    //             },
    //         },
    //     );
    //
    //     let mut decisions = vec![];
    //     let mut long_sma_tx_fees = vec![];
    //
    //     for height in (starting_block_height..=current_block_height).skip(long_sma as usize) {
    //         let fees = fee_analytics
    //             .calculate_sma(height - long_sma..=height)
    //             .await;
    //         let tx_fee = calculate_tx_fee(&fees);
    //         long_sma_tx_fees.push((height, tx_fee));
    //
    //         if decider
    //             .should_send_blob_tx(
    //                 6,
    //             Context {
    //                 at_l1_height: height,
    //                 num_l2_blocks_behind: (height - starting_block_height) * 12,
    //             },
    //         )
    //         .await
    //     {
    //         let current_fees = fee_lookup.get(&height).unwrap();
    //         let current_tx_fee = calculate_tx_fee(current_fees);
    //         decisions.push((height, current_tx_fee));
    //     }
    // }
    //
    // save_tx_fees(&long_sma_tx_fees, "long_sma_fees.csv");
    // save_tx_fees(&decisions, "decisions.csv");
    // }
}
