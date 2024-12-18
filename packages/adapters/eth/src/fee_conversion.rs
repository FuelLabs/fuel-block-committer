use std::ops::RangeInclusive;

use alloy::rpc::types::FeeHistory;
use itertools::{izip, Itertools};
use services::{
    fee_tracker::port::l1::{BlockFees, Fees},
    Result,
};

pub fn unpack_fee_history(fees: FeeHistory) -> Result<Vec<BlockFees>> {
    let number_of_blocks = if fees.base_fee_per_gas.is_empty() {
        0
    } else {
        // We subtract 1 because the last element is the expected fee for the next block
        fees.base_fee_per_gas
            .len()
            .checked_sub(1)
            .expect("checked not 0")
    };

    if number_of_blocks == 0 {
        return Ok(vec![]);
    }

    let Some(nested_rewards) = fees.reward.as_ref() else {
        return Err(services::Error::Other(format!(
            "missing rewards field: {fees:?}"
        )));
    };

    if number_of_blocks != nested_rewards.len()
        || number_of_blocks != fees.base_fee_per_blob_gas.len() - 1
    {
        return Err(services::Error::Other(format!(
            "discrepancy in lengths of fee fields: {fees:?}"
        )));
    }

    let rewards: Vec<_> = nested_rewards
        .iter()
        .map(|perc| {
            perc.last().copied().ok_or_else(|| {
                crate::error::Error::Other(
                    "should have had at least one reward percentile".to_string(),
                )
            })
        })
        .try_collect()?;

    let values = izip!(
        (fees.oldest_block..),
        fees.base_fee_per_gas.into_iter(),
        fees.base_fee_per_blob_gas.into_iter(),
        rewards
    )
    .take(number_of_blocks)
    .map(
        |(height, base_fee_per_gas, base_fee_per_blob_gas, reward)| BlockFees {
            height,
            fees: Fees {
                base_fee_per_gas,
                reward,
                base_fee_per_blob_gas,
            },
        },
    )
    .collect();

    Ok(values)
}

pub fn chunk_range_inclusive(
    initial_range: RangeInclusive<u64>,
    chunk_size: u64,
) -> Vec<std::ops::RangeInclusive<u64>> {
    let mut ranges = Vec::new();

    if chunk_size == 0 {
        return ranges;
    }

    let start = *initial_range.start();
    let end = *initial_range.end();

    let mut current = start;
    while current <= end {
        // Calculate the end of the current chunk.
        let chunk_end = (current + chunk_size - 1).min(end);

        ranges.push(current..=chunk_end);

        current = chunk_end + 1;
    }

    ranges
}

#[cfg(test)]
mod test {
    use alloy::rpc::types::FeeHistory;
    use services::fee_tracker::port::l1::{BlockFees, Fees};

    use std::ops::RangeInclusive;

    use crate::fee_conversion::{self};

    #[test]
    fn test_chunk_size_zero() {
        // given
        let initial_range = 1..=10;
        let chunk_size = 0;

        // when
        let result = fee_conversion::chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected: Vec<RangeInclusive<u64>> = vec![];
        assert_eq!(
            result, expected,
            "Expected empty vector when chunk_size is zero"
        );
    }

    #[test]
    fn test_chunk_size_larger_than_range() {
        // given
        let initial_range = 1..=5;
        let chunk_size = 10;

        // when
        let result = fee_conversion::chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![1..=5];
        assert_eq!(
            result, expected,
            "Expected single chunk when chunk_size exceeds range length"
        );
    }

    #[test]
    fn test_exact_multiples() {
        // given
        let initial_range = 1..=10;
        let chunk_size = 2;

        // when
        let result = fee_conversion::chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![1..=2, 3..=4, 5..=6, 7..=8, 9..=10];
        assert_eq!(result, expected, "Chunks should exactly divide the range");
    }

    #[test]
    fn test_non_exact_multiples() {
        // given
        let initial_range = 1..=10;
        let chunk_size = 3;

        // when
        let result = fee_conversion::chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![1..=3, 4..=6, 7..=9, 10..=10];
        assert_eq!(
            result, expected,
            "Last chunk should contain the remaining elements"
        );
    }

    #[test]
    fn test_single_element_range() {
        // given
        let initial_range = 5..=5;
        let chunk_size = 1;

        // when
        let result = fee_conversion::chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![5..=5];
        assert_eq!(
            result, expected,
            "Single element range should return one chunk with that element"
        );
    }

    #[test]
    fn test_start_equals_end_with_large_chunk_size() {
        // given
        let initial_range = 100..=100;
        let chunk_size = 50;

        // when
        let result = fee_conversion::chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![100..=100];
        assert_eq!(
            result, expected,
            "Single element range should return one chunk regardless of chunk_size"
        );
    }

    #[test]
    fn test_chunk_size_one() {
        // given
        let initial_range = 10..=15;
        let chunk_size = 1;

        // when
        let result = fee_conversion::chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![10..=10, 11..=11, 12..=12, 13..=13, 14..=14, 15..=15];
        assert_eq!(
            result, expected,
            "Each number should be its own chunk when chunk_size is one"
        );
    }

    #[test]
    fn test_full_range_chunk() {
        // given
        let initial_range = 20..=30;
        let chunk_size = 11;

        // when
        let result = fee_conversion::chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![20..=30];
        assert_eq!(
            result, expected,
            "Whole range should be a single chunk when chunk_size equals range size"
        );
    }

    #[test]
    fn test_unpack_fee_history_empty_base_fee() {
        // given
        let fees = FeeHistory {
            oldest_block: 100,
            base_fee_per_gas: vec![],
            base_fee_per_blob_gas: vec![],
            reward: Some(vec![]),
            ..Default::default()
        };

        // when
        let result = fee_conversion::unpack_fee_history(fees);

        // then
        let expected: Vec<BlockFees> = vec![];
        assert_eq!(
            result.unwrap(),
            expected,
            "Expected empty vector when base_fee_per_gas is empty"
        );
    }

    #[test]
    fn test_unpack_fee_history_missing_rewards() {
        // given
        let fees = FeeHistory {
            oldest_block: 200,
            base_fee_per_gas: vec![100, 200],
            base_fee_per_blob_gas: vec![150, 250],
            reward: None,
            ..Default::default()
        };

        // when
        let result = fee_conversion::unpack_fee_history(fees.clone());

        // then
        let expected_error = services::Error::Other(format!("missing rewards field: {:?}", fees));
        assert_eq!(
            result.unwrap_err(),
            expected_error,
            "Expected error due to missing rewards field"
        );
    }

    #[test]
    fn test_unpack_fee_history_discrepancy_in_lengths_base_fee_rewards() {
        // given
        let fees = FeeHistory {
            oldest_block: 300,
            base_fee_per_gas: vec![100, 200, 300],
            base_fee_per_blob_gas: vec![150, 250, 350],
            reward: Some(vec![vec![10]]), // Should have 2 rewards for 2 blocks
            ..Default::default()
        };

        // when
        let result = fee_conversion::unpack_fee_history(fees.clone());

        // then
        let expected_error =
            services::Error::Other(format!("discrepancy in lengths of fee fields: {:?}", fees));
        assert_eq!(
            result.unwrap_err(),
            expected_error,
            "Expected error due to discrepancy in lengths of fee fields"
        );
    }

    #[test]
    fn test_unpack_fee_history_discrepancy_in_lengths_blob_gas() {
        // given
        let fees = FeeHistory {
            oldest_block: 400,
            base_fee_per_gas: vec![100, 200, 300],
            base_fee_per_blob_gas: vec![150, 250], // Should have 3 elements
            reward: Some(vec![vec![10], vec![20]]),
            ..Default::default()
        };

        // when
        let result = fee_conversion::unpack_fee_history(fees.clone());

        // then
        let expected_error =
            services::Error::Other(format!("discrepancy in lengths of fee fields: {:?}", fees));
        assert_eq!(
            result.unwrap_err(),
            expected_error,
            "Expected error due to discrepancy in base_fee_per_blob_gas lengths"
        );
    }

    #[test]
    fn test_unpack_fee_history_empty_reward_percentile() {
        // given
        let fees = FeeHistory {
            oldest_block: 500,
            base_fee_per_gas: vec![100, 200],
            base_fee_per_blob_gas: vec![150, 250],
            reward: Some(vec![vec![]]), // Empty percentile
            ..Default::default()
        };

        // when
        let result = fee_conversion::unpack_fee_history(fees.clone());

        // then
        let expected_error =
            services::Error::Other("should have had at least one reward percentile".to_string());
        assert_eq!(
            result.unwrap_err(),
            expected_error,
            "Expected error due to empty reward percentile"
        );
    }

    #[test]
    fn test_unpack_fee_history_single_block() {
        // given
        let fees = FeeHistory {
            oldest_block: 600,
            base_fee_per_gas: vec![100, 200], // number_of_blocks =1
            base_fee_per_blob_gas: vec![150, 250],
            reward: Some(vec![vec![10]]),
            ..Default::default()
        };

        // when
        let result = fee_conversion::unpack_fee_history(fees);

        // then
        let expected = vec![BlockFees {
            height: 600,
            fees: Fees {
                base_fee_per_gas: 100,
                reward: 10,
                base_fee_per_blob_gas: 150,
            },
        }];
        assert_eq!(
            result.unwrap(),
            expected,
            "Expected one BlockFees entry for a single block"
        );
    }

    #[test]
    fn test_unpack_fee_history_multiple_blocks() {
        // given
        let fees = FeeHistory {
            oldest_block: 700,
            base_fee_per_gas: vec![100, 200, 300, 400], // number_of_blocks =3
            base_fee_per_blob_gas: vec![150, 250, 350, 450],
            reward: Some(vec![vec![10], vec![20], vec![30]]),
            ..Default::default()
        };

        // when
        let result = fee_conversion::unpack_fee_history(fees);

        // then
        let expected = vec![
            BlockFees {
                height: 700,
                fees: Fees {
                    base_fee_per_gas: 100,
                    reward: 10,
                    base_fee_per_blob_gas: 150,
                },
            },
            BlockFees {
                height: 701,
                fees: Fees {
                    base_fee_per_gas: 200,
                    reward: 20,
                    base_fee_per_blob_gas: 250,
                },
            },
            BlockFees {
                height: 702,
                fees: Fees {
                    base_fee_per_gas: 300,
                    reward: 30,
                    base_fee_per_blob_gas: 350,
                },
            },
        ];
        assert_eq!(
            result.unwrap(),
            expected,
            "Expected three BlockFees entries for three blocks"
        );
    }

    #[test]
    fn test_unpack_fee_history_large_values() {
        // given
        let fees = FeeHistory {
            oldest_block: u64::MAX - 2,
            base_fee_per_gas: vec![u128::MAX - 2, u128::MAX - 1, u128::MAX],
            base_fee_per_blob_gas: vec![u128::MAX - 3, u128::MAX - 2, u128::MAX - 1],
            reward: Some(vec![vec![u128::MAX - 4], vec![u128::MAX - 3]]),
            ..Default::default()
        };

        // when
        let result = fee_conversion::unpack_fee_history(fees.clone());

        // then
        let expected = vec![
            BlockFees {
                height: u64::MAX - 2,
                fees: Fees {
                    base_fee_per_gas: u128::MAX - 2,
                    reward: u128::MAX - 4,
                    base_fee_per_blob_gas: u128::MAX - 3,
                },
            },
            BlockFees {
                height: u64::MAX - 1,
                fees: Fees {
                    base_fee_per_gas: u128::MAX - 1,
                    reward: u128::MAX - 3,
                    base_fee_per_blob_gas: u128::MAX - 2,
                },
            },
        ];
        assert_eq!(
            result.unwrap(),
            expected,
            "Expected BlockFees entries with large u64 values"
        );
    }

    #[test]
    fn test_unpack_fee_history_full_range_chunk() {
        // given
        let fees = FeeHistory {
            oldest_block: 800,
            base_fee_per_gas: vec![500, 600, 700, 800, 900], // number_of_blocks =4
            base_fee_per_blob_gas: vec![550, 650, 750, 850, 950],
            reward: Some(vec![vec![50], vec![60], vec![70], vec![80]]),
            ..Default::default()
        };

        // when
        let result = fee_conversion::unpack_fee_history(fees);

        // then
        let expected = vec![
            BlockFees {
                height: 800,
                fees: Fees {
                    base_fee_per_gas: 500,
                    reward: 50,
                    base_fee_per_blob_gas: 550,
                },
            },
            BlockFees {
                height: 801,
                fees: Fees {
                    base_fee_per_gas: 600,
                    reward: 60,
                    base_fee_per_blob_gas: 650,
                },
            },
            BlockFees {
                height: 802,
                fees: Fees {
                    base_fee_per_gas: 700,
                    reward: 70,
                    base_fee_per_blob_gas: 750,
                },
            },
            BlockFees {
                height: 803,
                fees: Fees {
                    base_fee_per_gas: 800,
                    reward: 80,
                    base_fee_per_blob_gas: 850,
                },
            },
        ];
        assert_eq!(
            result.unwrap(),
            expected,
            "Expected BlockFees entries matching the full range chunk"
        );
    }
}
