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
        use std::{ops::RangeInclusive, sync::Arc};

        use mockall::predicate::*;
        use tokio::sync::Barrier;

        use crate::fee_metrics_tracker::port::{
            cache::CachingApi,
            l1::{BlockFees, Fees, MockApi, SequentialBlockFees},
        };

        /// Helper function to generate sequential fees for a given range
        fn generate_sequential_fees(height_range: RangeInclusive<u64>) -> SequentialBlockFees {
            height_range
                .map(|h| {
                    let fee = u128::from(h) + 1;
                    BlockFees {
                        height: h,
                        fees: Fees {
                            base_fee_per_gas: fee,
                            reward: fee,
                            base_fee_per_blob_gas: fee,
                        },
                    }
                })
                .collect::<Result<_, _>>()
                .unwrap()
        }

        #[tokio::test]
        async fn caching_provider_avoids_duplicate_requests() {
            // Arrange
            let mut mock_provider = MockApi::new();

            mock_provider
                .expect_fees()
                .with(eq(0..=4))
                .once()
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            mock_provider
                .expect_current_height()
                .returning(|| Box::pin(async { Ok(10) }));

            let provider = CachingApi::new(mock_provider, 10);

            // Act
            let first_call = provider.get_fees(0..=4).await.unwrap();
            let second_call = provider.get_fees(0..=4).await.unwrap();

            // Assert
            assert_eq!(first_call, second_call);
        }

        #[tokio::test]
        async fn caching_provider_fetches_only_missing_blocks() {
            // Arrange
            let mut mock_provider = MockApi::new();

            let mut sequence = mockall::Sequence::new();

            mock_provider
                .expect_fees()
                .with(eq(0..=2))
                .once()
                .in_sequence(&mut sequence)
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            mock_provider
                .expect_fees()
                .with(eq(3..=5))
                .once()
                .in_sequence(&mut sequence)
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            mock_provider
                .expect_current_height()
                .returning(|| Box::pin(async { Ok(10) }));

            let provider = CachingApi::new(mock_provider, 10);

            // Act
            let first_call = provider.get_fees(0..=2).await.unwrap();
            let second_call = provider.get_fees(0..=5).await.unwrap();

            // Assert
            // First call fetches 0..=2
            // Second call fetches 3..=5 since 2 is already cached
            assert_eq!(first_call, generate_sequential_fees(0..=2));
            let expected_second = generate_sequential_fees(0..=5);
            assert_eq!(second_call, expected_second);
        }

        #[tokio::test]
        async fn caching_provider_evicts_oldest_blocks() {
            // Arrange
            let mut mock_provider = MockApi::new();

            mock_provider
                .expect_fees()
                .with(eq(0..=4))
                .times(2)
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            mock_provider
                .expect_fees()
                .with(eq(5..=9))
                .once()
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            mock_provider
                .expect_current_height()
                .returning(|| Box::pin(async { Ok(10) }));

            let provider = CachingApi::new(mock_provider, 5);

            // Act
            let first_call = provider.get_fees(0..=4).await.unwrap();
            let second_call = provider.get_fees(5..=9).await.unwrap();
            let _third_call = provider.get_fees(0..=4).await.unwrap();

            // Assert
            assert_eq!(first_call, generate_sequential_fees(0..=4));
            assert_eq!(second_call, generate_sequential_fees(5..=9));
            // Since cache limit is 5, the first range 0..=4 should have been evicted
            // Thus, the third call should refetch 0..=4
        }

        #[tokio::test]
        async fn caching_provider_handles_request_larger_than_cache() {
            // Arrange
            let mut mock_provider = MockApi::new();

            let cache_limit = 5;

            mock_provider
                .expect_fees()
                .with(eq(0..=9))
                .once()
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            mock_provider
                .expect_current_height()
                .returning(|| Box::pin(async { Ok(10) }));

            let provider = CachingApi::new(mock_provider, cache_limit);

            // Act
            let result = provider.get_fees(0..=9).await.unwrap();

            // Assert
            assert_eq!(result, generate_sequential_fees(0..=9));
        }

        #[tokio::test]
        async fn caching_provider_handles_concurrent_requests() {
            // Arrange
            let mut mock_provider = MockApi::new();

            let cache_limit = 10;

            // Use a barrier to synchronize the mock calls
            let barrier = Arc::new(Barrier::new(2));

            let barrier_clone = barrier.clone();

            mock_provider
                .expect_fees()
                .with(eq(0..=4))
                .once()
                .returning(move |range| {
                    let barrier = barrier_clone.clone();
                    Box::pin(async move {
                        // Wait for the second request to start
                        barrier.wait().await;
                        Ok(generate_sequential_fees(range))
                    })
                });

            mock_provider
                .expect_current_height()
                .returning(|| Box::pin(async { Ok(10) }));

            let provider = Arc::new(CachingApi::new(mock_provider, cache_limit));

            // Act
            let provider_clone = provider.clone();
            let handle1 =
                tokio::spawn(async move { provider_clone.get_fees(0..=4).await.unwrap() });

            let provider_clone = provider.clone();
            let handle2 = tokio::spawn(async move {
                // Ensure both tasks start around the same time
                barrier.wait().await;
                provider_clone.get_fees(0..=4).await.unwrap()
            });

            let result1 = handle1.await.unwrap();
            let result2 = handle2.await.unwrap();

            // Assert
            assert_eq!(result1, result2);
        }

        #[tokio::test]
        async fn caching_provider_import_and_export() {
            // Arrange
            let mock_provider = MockApi::new();

            let cache_limit = 10;

            let provider = CachingApi::new(mock_provider, cache_limit);

            let fees_to_import = (0..5)
                .map(|h| {
                    let fee = u128::from(h) + 1;
                    (
                        h,
                        Fees {
                            base_fee_per_gas: fee,
                            reward: fee,
                            base_fee_per_blob_gas: fee,
                        },
                    )
                })
                .collect::<Vec<_>>();

            // Act
            provider.import(fees_to_import.clone()).await;
            let exported = provider.export().await.into_iter().collect::<Vec<_>>();

            // Assert
            assert_eq!(exported, fees_to_import);
        }

        #[tokio::test]
        async fn caching_provider_handles_single_element_range() {
            // Arrange
            let mut mock_provider = MockApi::new();

            mock_provider
                .expect_fees()
                .with(eq(3..=3))
                .once()
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            mock_provider
                .expect_current_height()
                .returning(|| Box::pin(async { Ok(10) }));

            let provider = CachingApi::new(mock_provider, 10);

            // Act
            let result = provider.get_fees(3..=3).await.unwrap();

            // Assert
            let expected = generate_sequential_fees(3..=3);
            assert_eq!(result, expected);
        }

        #[tokio::test]
        async fn caching_provider_handles_overlapping_ranges() {
            // Arrange
            let mut mock_provider = MockApi::new();

            let mut sequence = mockall::Sequence::new();

            // First fetch 0..=4
            mock_provider
                .expect_fees()
                .with(eq(0..=4))
                .once()
                .in_sequence(&mut sequence)
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            // Then fetch 3..=7 (only 5..=7 should be fetched)
            mock_provider
                .expect_fees()
                .with(eq(5..=7))
                .once()
                .in_sequence(&mut sequence)
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            mock_provider
                .expect_current_height()
                .returning(|| Box::pin(async { Ok(10) }));

            let provider = CachingApi::new(mock_provider, 10);

            // Act
            let first_call = provider.get_fees(0..=4).await.unwrap();
            let second_call = provider.get_fees(0..=7).await.unwrap();

            // Assert
            let expected_first = generate_sequential_fees(0..=4);
            let expected_second = generate_sequential_fees(0..=7);
            assert_eq!(first_call, expected_first);
            assert_eq!(second_call, expected_second);
        }

        #[tokio::test]
        async fn caching_provider_export_after_import() {
            // Arrange
            let mut mock_provider = MockApi::new();

            mock_provider
                .expect_current_height()
                .returning(|| Box::pin(async { Ok(10) }));

            let provider = CachingApi::new(mock_provider, 10);

            let fees_to_import = (10..=14)
                .map(|h| {
                    let fee = u128::from(h) + 1;
                    (
                        h,
                        Fees {
                            base_fee_per_gas: fee,
                            reward: fee,
                            base_fee_per_blob_gas: fee,
                        },
                    )
                })
                .collect::<Vec<_>>();

            // Act
            provider.import(fees_to_import.clone()).await;
            let exported_fees = provider.export().await.into_iter().collect::<Vec<_>>();

            // Assert
            assert_eq!(exported_fees, fees_to_import);
        }

        #[tokio::test]
        async fn caching_provider_updates_cache_correctly() {
            // Arrange
            let mut mock_provider = MockApi::new();

            mock_provider
                .expect_fees()
                .with(eq(0..=4))
                .once()
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            mock_provider
                .expect_fees()
                .with(eq(5..=5))
                .once()
                .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

            mock_provider
                .expect_current_height()
                .returning(|| Box::pin(async { Ok(10) }));

            let provider = CachingApi::new(mock_provider, 6);

            // Act
            let first_call = provider.get_fees(0..=4).await.unwrap();
            let second_call = provider.get_fees(0..=5).await.unwrap();

            let exported = provider.export().await.into_iter().collect::<Vec<_>>();

            // Assert
            let expected_first = generate_sequential_fees(0..=4);
            let expected_second = generate_sequential_fees(0..=5);
            assert_eq!(first_call, expected_first);
            assert_eq!(second_call, expected_second);
            assert_eq!(exported.len(), 6);
            assert_eq!(
                exported,
                generate_sequential_fees(0..=5)
                    .into_iter()
                    .map(|bf| (bf.height, bf.fees))
                    .collect::<Vec<_>>()
            );
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
            let mut fees = vec![];
            for range in detect_missing_ranges(available_fees, height_range) {
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

    fn detect_missing_ranges(
        available_fees: &[BlockFees],
        height_range: RangeInclusive<u64>,
    ) -> Vec<RangeInclusive<u64>> {
        if available_fees.is_empty() {
            return vec![height_range];
        }

        let (last_height, mut missing_ranges) = available_fees.iter().map(|bf| bf.height).fold(
            (None, Vec::new()),
            |(prev, mut acc), current| {
                match prev {
                    Some(prev_h) => {
                        if current > prev_h + 1 {
                            // Found a gap between prev_h and current
                            acc.push((prev_h + 1)..=current.saturating_sub(1));
                        }
                    }
                    None => {
                        if current > *height_range.start() {
                            // Missing range before the first available height
                            acc.push(*height_range.start()..=current.saturating_sub(1));
                        }
                    }
                }
                (Some(current), acc)
            },
        );

        // Check for a missing range after the last available height
        if let Some(last_h) = last_height {
            if last_h < *height_range.end() {
                missing_ranges.push((last_h + 1)..=*height_range.end());
            }
        }

        missing_ranges
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
