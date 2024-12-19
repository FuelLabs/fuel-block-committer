pub mod l1 {
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
    use std::ops::RangeInclusive;

    use itertools::Itertools;

    #[derive(Debug, PartialEq, Eq)]
    pub struct SequentialBlockFees {
        fees: Vec<BlockFees>,
    }

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

    // Cannot be empty
    #[allow(clippy::len_without_is_empty)]
    impl SequentialBlockFees {
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

pub mod cache {
    use std::{collections::BTreeMap, ops::RangeInclusive, sync::Arc};

    use tokio::sync::RwLock;

    use crate::Error;

    use super::l1::{Api, BlockFees, Fees, SequentialBlockFees};

    #[derive(Debug, Clone)]
    pub struct CachingApi<P> {
        fees_provider: P,
        cache: Arc<RwLock<BTreeMap<u64, Fees>>>,
        cache_limit: usize,
    }

    impl<P> CachingApi<P> {
        pub fn new(fees_provider: P, cache_limit: usize) -> Self {
            Self {
                fees_provider,
                cache: Arc::new(RwLock::new(BTreeMap::new())),
                cache_limit,
            }
        }
    }

    impl<P: Api + Send + Sync> Api for CachingApi<P> {
        async fn fees(
            &self,
            height_range: RangeInclusive<u64>,
        ) -> crate::Result<SequentialBlockFees> {
            self.get_fees(height_range).await
        }

        async fn current_height(&self) -> crate::Result<u64> {
            self.fees_provider.current_height().await
        }
    }

    impl<P: Api> CachingApi<P> {
        pub async fn get_fees(
            &self,
            height_range: RangeInclusive<u64>,
        ) -> crate::Result<SequentialBlockFees> {
            let mut missing_heights = vec![];

            // Mind the scope to release the read lock
            {
                let cache = self.cache.read().await;
                for height in height_range.clone() {
                    if !cache.contains_key(&height) {
                        missing_heights.push(height);
                    }
                }
            }

            if !missing_heights.is_empty() {
                let fetched_fees = self
                    .fees_provider
                    .fees(
                        *missing_heights.first().expect("not empty")
                            ..=*missing_heights.last().expect("not empty"),
                    )
                    .await?;

                let mut cache = self.cache.write().await;
                for block_fee in fetched_fees {
                    cache.insert(block_fee.height, block_fee.fees);
                }
            }

            let fees: Vec<_> = {
                let cache = self.cache.read().await;
                height_range
                    .filter_map(|h| {
                        cache.get(&h).map(|f| BlockFees {
                            height: h,
                            fees: *f,
                        })
                    })
                    .collect()
            };

            self.shrink_cache().await;

            SequentialBlockFees::try_from(fees).map_err(|e| Error::Other(e.to_string()))
        }

        async fn shrink_cache(&self) {
            let mut cache = self.cache.write().await;
            while cache.len() > self.cache_limit {
                cache.pop_first();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::ops::RangeInclusive;

        use mockall::{predicate::eq, Sequence};

        use crate::fee_tracker::port::{
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
                .return_once(|range| {
                    Box::pin(async move {
                        Ok(SequentialBlockFees::try_from(
                            range
                                .map(|h| BlockFees {
                                    height: h,
                                    fees: Fees {
                                        base_fee_per_gas: h as u128,
                                        reward: h as u128,
                                        base_fee_per_blob_gas: h as u128,
                                    },
                                })
                                .collect::<Vec<_>>(),
                        )
                        .unwrap())
                    })
                });

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
                .return_once(|range| {
                    Box::pin(async move {
                        Ok(SequentialBlockFees::try_from(
                            range
                                .map(|h| BlockFees {
                                    height: h,
                                    fees: Fees {
                                        base_fee_per_gas: h as u128,
                                        reward: h as u128,
                                        base_fee_per_blob_gas: h as u128,
                                    },
                                })
                                .collect::<Vec<_>>(),
                        )
                        .unwrap())
                    })
                })
                .in_sequence(&mut sequence);

            mock_provider
                .expect_fees()
                .with(eq(3..=5))
                .once()
                .return_once(|range| {
                    Box::pin(async move {
                        Ok(SequentialBlockFees::try_from(
                            range
                                .map(|h| BlockFees {
                                    height: h,
                                    fees: Fees {
                                        base_fee_per_gas: h as u128,
                                        reward: h as u128,
                                        base_fee_per_blob_gas: h as u128,
                                    },
                                })
                                .collect::<Vec<_>>(),
                        )
                        .unwrap())
                    })
                })
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
                    .map(|h| BlockFees {
                        height: h,
                        fees: Fees {
                            base_fee_per_gas: h as u128,
                            reward: h as u128,
                            base_fee_per_blob_gas: h as u128,
                        },
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        }
    }
}
