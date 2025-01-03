pub mod l1 {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Fees {
        pub base_fee_per_gas: u128,
        pub reward: u128,
        pub base_fee_per_blob_gas: u128,
    }

    impl Default for Fees {
        fn default() -> Self {
            Fees {
                base_fee_per_gas: 1.try_into().unwrap(),
                reward: 1.try_into().unwrap(),
                base_fee_per_blob_gas: 1.try_into().unwrap(),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct BlockFees {
        pub height: u64,
        pub fees: Fees,
    }
    use std::ops::RangeInclusive;

    use itertools::Itertools;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Eq, Clone)]
    pub struct SequentialBlockFees {
        fees: Vec<BlockFees>,
    }

    // Doesn't detect that we use the contents in the Display impl
    #[allow(dead_code)]
    #[derive(Debug)]
    pub struct InvalidSequence(String);

    impl std::error::Error for InvalidSequence {}

    impl std::fmt::Display for InvalidSequence {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }

    impl IntoIterator for SequentialBlockFees {
        type Item = BlockFees;
        type IntoIter = std::vec::IntoIter<BlockFees>;
        fn into_iter(self) -> Self::IntoIter {
            self.fees.into_iter()
        }
    }

    impl FromIterator<BlockFees> for Result<SequentialBlockFees, InvalidSequence> {
        fn from_iter<T: IntoIterator<Item = BlockFees>>(iter: T) -> Self {
            SequentialBlockFees::try_from(iter.into_iter().collect::<Vec<_>>())
        }
    }

    // Cannot be empty
    #[allow(clippy::len_without_is_empty)]
    impl SequentialBlockFees {
        pub fn last(&self) -> &BlockFees {
            self.fees.last().expect("not empty")
        }

        pub fn mean(&self) -> Fees {
            let count = self.len() as u128;

            let total = self
                .fees
                .iter()
                .map(|bf| bf.fees)
                .fold(Fees::default(), |acc, f| {
                    let base_fee_per_gas = acc.base_fee_per_gas.saturating_add(f.base_fee_per_gas);
                    let reward = acc.reward.saturating_add(f.reward);
                    let base_fee_per_blob_gas = acc
                        .base_fee_per_blob_gas
                        .saturating_add(f.base_fee_per_blob_gas);

                    Fees {
                        base_fee_per_gas,
                        reward,
                        base_fee_per_blob_gas,
                    }
                });

            Fees {
                base_fee_per_gas: total.base_fee_per_gas.saturating_div(count),
                reward: total.reward.saturating_div(count),
                base_fee_per_blob_gas: total.base_fee_per_blob_gas.saturating_div(count),
            }
        }
        pub fn len(&self) -> usize {
            self.fees.len()
        }

        pub fn height_range(&self) -> RangeInclusive<u64> {
            let start = self.fees.first().expect("not empty").height;
            let end = self.fees.last().expect("not empty").height;
            start..=end
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
    pub trait Api {
        async fn fees(
            &self,
            height_range: RangeInclusive<u64>,
        ) -> crate::Result<SequentialBlockFees>;
        async fn current_height(&self) -> crate::Result<u64>;
    }

    #[cfg(feature = "test-helpers")]
    pub mod testing {

        use std::{collections::BTreeMap, ops::RangeInclusive};

        use itertools::Itertools;

        use super::{Api, BlockFees, Fees, SequentialBlockFees};

        #[derive(Debug, Clone, Copy)]
        pub struct ConstantFeeApi {
            fees: Fees,
        }

        impl ConstantFeeApi {
            pub fn new(fees: Fees) -> Self {
                Self { fees }
            }
        }

        impl Api for ConstantFeeApi {
            async fn fees(
                &self,
                height_range: RangeInclusive<u64>,
            ) -> crate::Result<SequentialBlockFees> {
                let fees = height_range
                    .into_iter()
                    .map(|height| BlockFees {
                        height,
                        fees: self.fees,
                    })
                    .collect_vec();

                Ok(fees.try_into().unwrap())
            }

            async fn current_height(&self) -> crate::Result<u64> {
                Ok(0)
            }
        }

        #[derive(Debug, Clone)]
        pub struct PreconfiguredFeeApi {
            fees: BTreeMap<u64, Fees>,
        }

        impl Api for PreconfiguredFeeApi {
            async fn current_height(&self) -> crate::Result<u64> {
                Ok(*self
                    .fees
                    .keys()
                    .last()
                    .expect("no fees registered with PreconfiguredFeesProvider"))
            }

            async fn fees(
                &self,
                height_range: RangeInclusive<u64>,
            ) -> crate::Result<SequentialBlockFees> {
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

                Ok(fees.try_into().expect("block fees not sequential"))
            }
        }

        impl PreconfiguredFeeApi {
            pub fn new(blocks: impl IntoIterator<Item = (u64, Fees)>) -> Self {
                Self {
                    fees: blocks.into_iter().collect(),
                }
            }
        }

        pub fn incrementing_fees(num_blocks: u64) -> SequentialBlockFees {
            let fees = (0..num_blocks)
                .map(|i| {
                    let fee = u128::from(i) + 1;
                    BlockFees {
                        height: i,
                        fees: Fees {
                            base_fee_per_gas: fee,
                            reward: fee,
                            base_fee_per_blob_gas: fee,
                        },
                    }
                })
                .collect::<Result<_, _>>();

            fees.unwrap()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

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
            let mean = sequential_fees.mean();

            // then
            assert_eq!(
                mean.base_fee_per_gas, 1,
                "base_fee_per_gas should be set to 1 when total is 0"
            );
            assert_eq!(mean.reward, 1, "reward should be set to 1 when total is 0");
            assert_eq!(
                mean.base_fee_per_blob_gas, 1,
                "base_fee_per_blob_gas should be set to 1 when total is 0"
            );
        }

        #[tokio::test]
        async fn calculates_sma_correctly() {
            // given
            let sut = testing::incrementing_fees(5);

            // when
            let sma = sut.mean();

            // then
            assert_eq!(sma.base_fee_per_gas, 6);
            assert_eq!(sma.reward, 6);
            assert_eq!(sma.base_fee_per_blob_gas, 6);
        }
    }
}

pub mod cache {
    use std::{collections::BTreeMap, ops::RangeInclusive, sync::Arc};

    use tokio::sync::Mutex;

    use super::l1::{Api, BlockFees, Fees, SequentialBlockFees};
    use crate::{Error, Result};

    #[derive(Debug, Clone)]
    pub struct CachingApi<P> {
        fees_provider: P,
        // preferred over RwLock because of simplicity
        cache: Arc<Mutex<BTreeMap<u64, Fees>>>,
        cache_limit: usize,
    }

    impl<P> CachingApi<P> {
        pub fn inner(&self) -> &P {
            &self.fees_provider
        }

        pub fn new(fees_provider: P, cache_limit: usize) -> Self {
            Self {
                fees_provider,
                cache: Arc::new(Mutex::new(BTreeMap::new())),
                cache_limit,
            }
        }

        pub async fn import(&self, fees: impl IntoIterator<Item = (u64, Fees)>) {
            self.cache.lock().await.extend(fees);
        }

        pub async fn export(&self) -> impl IntoIterator<Item = (u64, Fees)> {
            self.cache.lock().await.clone()
        }
    }

    impl<P: Api + Send + Sync> Api for CachingApi<P> {
        async fn fees(&self, height_range: RangeInclusive<u64>) -> Result<SequentialBlockFees> {
            self.get_fees(height_range).await
        }

        async fn current_height(&self) -> Result<u64> {
            self.fees_provider.current_height().await
        }
    }

    impl<P: Api> CachingApi<P> {
        async fn download_missing_fees(
            &self,
            available_fees: &[BlockFees],
            height_range: RangeInclusive<u64>,
        ) -> Result<Vec<BlockFees>> {
            let mut missing_ranges = vec![];
            {
                let present_heights = available_fees.iter().map(|bf| bf.height);
                let mut expected_heights = height_range.clone();

                let mut last_present_height = None;
                for actual_height in present_heights {
                    last_present_height = Some(actual_height);

                    let next_expected = expected_heights
                        .next()
                        .expect("can never have less values than `present_heights`");

                    if actual_height != next_expected {
                        missing_ranges.push(next_expected..=actual_height.saturating_sub(1));
                    }
                }

                if let Some(height) = last_present_height {
                    if height != *height_range.end() {
                        missing_ranges.push(height..=*height_range.end())
                    }
                } else {
                    missing_ranges.push(height_range.clone());
                }
            }

            let mut fees = vec![];
            for range in missing_ranges {
                let new_fees = self.fees_provider.fees(range.clone()).await?;
                fees.extend(new_fees);
            }

            Ok(fees)
        }

        pub async fn get_fees(
            &self,
            height_range: RangeInclusive<u64>,
        ) -> Result<SequentialBlockFees> {
            let mut cache_guard = self.cache.lock().await;
            let mut fees = Self::read_cached_fees(&cache_guard, height_range.clone()).await;
            let missing_fees = self.download_missing_fees(&fees, height_range).await?;

            self.update_cache(&mut cache_guard, missing_fees.clone())
                .await;
            drop(cache_guard);

            fees.extend(missing_fees);
            fees.sort_by_key(|f| f.height);

            SequentialBlockFees::try_from(fees).map_err(|e| Error::Other(e.to_string()))
        }

        async fn read_cached_fees(
            cache: &BTreeMap<u64, Fees>,
            height_range: RangeInclusive<u64>,
        ) -> Vec<BlockFees> {
            cache
                .range(height_range)
                .map(|(height, fees)| BlockFees {
                    height: *height,
                    fees: *fees,
                })
                .collect()
        }

        async fn update_cache(&self, cache: &mut BTreeMap<u64, Fees>, fees: Vec<BlockFees>) {
            cache.extend(fees.into_iter().map(|bf| (bf.height, bf.fees)));

            while cache.len() > self.cache_limit {
                cache.pop_first();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::ops::RangeInclusive;

        use mockall::{predicate::eq, Sequence};

        use crate::fee_metrics_tracker::port::{
            cache::CachingApi,
            l1::{BlockFees, Fees, MockApi, SequentialBlockFees},
        };

        #[tokio::test]
        async fn caching_provider_avoids_duplicate_requests() {
            // given
            let mut mock_provider = MockApi::new();

            mock_provider
                .expect_fees()
                .with(eq(0..=4))
                .once()
                .return_once(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            let provider = CachingApi::new(mock_provider, 5);
            let _ = provider.get_fees(0..=4).await.unwrap();

            // when
            let _ = provider.get_fees(0..=4).await.unwrap();

            // then
            // mock validates no extra calls made
        }

        #[tokio::test]
        async fn caching_provider_fetches_only_missing_blocks() {
            // given
            let mut mock_provider = MockApi::new();

            let mut sequence = Sequence::new();
            mock_provider
                .expect_fees()
                .with(eq(0..=2))
                .once()
                .return_once(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }))
                .in_sequence(&mut sequence);

            mock_provider
                .expect_fees()
                .with(eq(3..=5))
                .once()
                .return_once(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }))
                .in_sequence(&mut sequence);

            let provider = CachingApi::new(mock_provider, 5);
            let _ = provider.get_fees(0..=2).await.unwrap();

            // when
            let _ = provider.get_fees(2..=5).await.unwrap();

            // then
            // not called for the overlapping area
        }

        #[tokio::test]
        async fn caching_provider_evicts_oldest_blocks() {
            // given
            let mut mock_provider = MockApi::new();

            mock_provider
                .expect_fees()
                .with(eq(0..=4))
                .times(2)
                .returning(|range| Box::pin(async { Ok(generate_sequential_fees(range)) }));

            mock_provider
                .expect_fees()
                .with(eq(5..=9))
                .times(1)
                .returning(|range| Box::pin(async { Ok(generate_sequential_fees(range)) }));

            let provider = CachingApi::new(mock_provider, 5);
            let _ = provider.get_fees(0..=4).await.unwrap();
            let _ = provider.get_fees(5..=9).await.unwrap();

            // when
            let _ = provider.get_fees(0..=4).await.unwrap();

            // then
            // will refetch 0..=4 due to eviction
        }

        #[tokio::test]
        async fn caching_provider_handles_request_larger_than_cache() {
            use mockall::predicate::*;

            // given
            let mut mock_provider = MockApi::new();

            let cache_limit = 5;

            mock_provider
                .expect_fees()
                .with(eq(0..=9))
                .times(1)
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            let provider = CachingApi::new(mock_provider, cache_limit);

            // when
            let result = provider.get_fees(0..=9).await.unwrap();

            assert_eq!(result, generate_sequential_fees(0..=9));
        }

        fn generate_sequential_fees(height_range: RangeInclusive<u64>) -> SequentialBlockFees {
            SequentialBlockFees::try_from(
                height_range
                    .map(|h| {
                        let fee = u128::from(h + 1);
                        BlockFees {
                            height: h,
                            fees: Fees {
                                base_fee_per_gas: fee,
                                reward: fee,
                                base_fee_per_blob_gas: fee,
                            },
                        }
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        }
    }
}
