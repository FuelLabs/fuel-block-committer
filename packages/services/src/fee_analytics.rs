pub mod port {
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Fees {
        pub base_fee_per_gas: u128,
        pub reward: u128,
        pub base_fee_per_blob_gas: u128,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BlockFees {
        pub height: u64,
        pub fees: Fees,
    }

    pub mod l1 {
        use std::ops::RangeInclusive;

        use itertools::Itertools;
        use nonempty::NonEmpty;

        use super::BlockFees;

        #[derive(Debug)]
        pub struct SequentialBlockFees {
            fees: Vec<BlockFees>,
        }

        impl IntoIterator for SequentialBlockFees {
            type Item = BlockFees;
            type IntoIter = std::vec::IntoIter<BlockFees>;
            fn into_iter(self) -> Self::IntoIter {
                self.fees.into_iter()
            }
        }

        // Cannot be empty
        #[allow(clippy::len_without_is_empty)]
        impl SequentialBlockFees {
            pub fn len(&self) -> usize {
                self.fees.len()
            }
        }

        #[derive(Debug)]
        pub struct InvalidSequence(String);

        impl std::error::Error for InvalidSequence {}

        impl std::fmt::Display for InvalidSequence {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{self:?}")
            }
        }

        impl TryFrom<Vec<BlockFees>> for SequentialBlockFees {
            type Error = InvalidSequence;
            fn try_from(mut fees: Vec<BlockFees>) -> Result<Self, Self::Error> {
                if fees.is_empty() {
                    return Err(InvalidSequence("Input cannot be empty".to_string()));
                }

                fees.sort_by_key(|f| f.height);

                let is_sequential = fees
                    .iter()
                    .tuple_windows()
                    .all(|(l, r)| l.height + 1 == r.height);

                let heights = fees.iter().map(|f| f.height).collect::<Vec<_>>();
                if !is_sequential {
                    return Err(InvalidSequence(format!(
                        "blocks are not sequential by height: {heights:?}"
                    )));
                }

                Ok(Self { fees })
            }
        }

        #[allow(async_fn_in_trait)]
        #[trait_variant::make(Send)]
        #[cfg_attr(feature = "test-helpers", mockall::automock)]
        pub trait FeesProvider {
            async fn fees(&self, height_range: RangeInclusive<u64>) -> SequentialBlockFees;
            async fn current_block_height(&self) -> u64;
        }

        #[cfg(feature = "test-helpers")]
        pub mod testing {
            use std::{collections::BTreeMap, ops::RangeInclusive};

            use itertools::Itertools;
            use nonempty::NonEmpty;

            use crate::{
                fee_analytics::port::{BlockFees, Fees},
                types::CollectNonEmpty,
            };

            use super::{FeesProvider, SequentialBlockFees};

            #[derive(Debug, Clone)]
            pub struct TestFeesProvider {
                fees: BTreeMap<u64, Fees>,
            }

            impl FeesProvider for TestFeesProvider {
                async fn current_block_height(&self) -> u64 {
                    *self.fees.keys().last().unwrap()
                }

                async fn fees(&self, height_range: RangeInclusive<u64>) -> SequentialBlockFees {
                    let fees = self
                        .fees
                        .iter()
                        .skip_while(|(height, _)| !height_range.contains(height))
                        .take_while(|(height, _)| height_range.contains(height))
                        .map(|(height, fees)| BlockFees {
                            height: *height,
                            fees: *fees,
                        })
                        .collect_vec();

                    fees.try_into().unwrap()
                }
            }

            impl TestFeesProvider {
                pub fn new(blocks: impl IntoIterator<Item = (u64, Fees)>) -> Self {
                    Self {
                        fees: blocks.into_iter().collect(),
                    }
                }
            }

            pub fn incrementing_fees(num_blocks: u64) -> BTreeMap<u64, Fees> {
                (0..num_blocks)
                    .map(|i| {
                        (
                            i,
                            Fees {
                                base_fee_per_gas: i as u128 + 1,
                                reward: i as u128 + 1,
                                base_fee_per_blob_gas: i as u128 + 1,
                            },
                        )
                    })
                    .collect()
            }
        }
    }
}

pub mod service {

    use std::ops::RangeInclusive;

    use nonempty::NonEmpty;

    use super::port::{
        l1::{FeesProvider, SequentialBlockFees},
        Fees,
    };

    pub struct FeeAnalytics<P> {
        fees_provider: P,
    }
    impl<P> FeeAnalytics<P> {
        pub fn new(fees_provider: P) -> Self {
            Self { fees_provider }
        }
    }

    impl<P: FeesProvider> FeeAnalytics<P> {
        // TODO: segfault fail or signal if missing blocks/holes present
        // TODO: segfault cache fees/save to db
        // TODO: segfault job to update fees in the background
        pub async fn calculate_sma(&self, block_range: RangeInclusive<u64>) -> Fees {
            let fees = self.fees_provider.fees(block_range).await;

            Self::mean(fees)
        }

        fn mean(fees: SequentialBlockFees) -> Fees {
            let count = fees.len() as u128;

            let total = fees
                .into_iter()
                .map(|bf| bf.fees)
                .fold(Fees::default(), |acc, f| Fees {
                    base_fee_per_gas: acc.base_fee_per_gas + f.base_fee_per_gas,
                    reward: acc.reward + f.reward,
                    base_fee_per_blob_gas: acc.base_fee_per_blob_gas + f.base_fee_per_blob_gas,
                });

            // TODO: segfault should we round to nearest here?
            Fees {
                base_fee_per_gas: total.base_fee_per_gas.saturating_div(count),
                reward: total.reward.saturating_div(count),
                base_fee_per_blob_gas: total.base_fee_per_blob_gas.saturating_div(count),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use itertools::Itertools;
    use port::{l1::SequentialBlockFees, BlockFees, Fees};

    use super::*;

    #[test]
    fn can_create_valid_sequential_fees() {
        // Given
        let block_fees = vec![
            BlockFees {
                height: 1,
                fees: Fees {
                    base_fee_per_gas: 100,
                    reward: 50,
                    base_fee_per_blob_gas: 10,
                },
            },
            BlockFees {
                height: 2,
                fees: Fees {
                    base_fee_per_gas: 110,
                    reward: 55,
                    base_fee_per_blob_gas: 15,
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
                    base_fee_per_gas: 100,
                    reward: 50,
                    base_fee_per_blob_gas: 10,
                },
            },
            BlockFees {
                height: 3, // Non-sequential height
                fees: Fees {
                    base_fee_per_gas: 110,
                    reward: 55,
                    base_fee_per_blob_gas: 15,
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

    // TODO: segfault add more tests so that the in-order iteration invariant is properly tested
    #[test]
    fn produced_iterator_gives_correct_values() {
        // Given
        // notice the heights are out of order so that we validate that the returned sequence is in
        // order
        let block_fees = vec![
            BlockFees {
                height: 2,
                fees: Fees {
                    base_fee_per_gas: 110,
                    reward: 55,
                    base_fee_per_blob_gas: 15,
                },
            },
            BlockFees {
                height: 1,
                fees: Fees {
                    base_fee_per_gas: 100,
                    reward: 50,
                    base_fee_per_blob_gas: 10,
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
}
