use std::{collections::BTreeMap, ops::RangeInclusive};

use itertools::Itertools;
use tokio::sync::RwLock;

use crate::state_committer::port::l1::Api;

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

#[derive(Debug, PartialEq, Eq)]
pub struct SequentialBlockFees {
    fees: Vec<BlockFees>,
}

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait FeesProvider {
    async fn fees(&self, height_range: RangeInclusive<u64>) -> crate::Result<SequentialBlockFees>;
    async fn current_block_height(&self) -> crate::Result<u64>;
}

#[derive(Debug)]
pub struct CachingFeesProvider<P> {
    fees_provider: P,
    cache: RwLock<BTreeMap<u64, Fees>>,
    cache_limit: usize,
}

impl<P> CachingFeesProvider<P> {
    pub fn new(fees_provider: P, cache_limit: usize) -> Self {
        Self {
            fees_provider,
            cache: RwLock::new(BTreeMap::new()),
            cache_limit,
        }
    }
}

impl<P: FeesProvider + Send + Sync> FeesProvider for CachingFeesProvider<P> {
    async fn fees(&self, height_range: RangeInclusive<u64>) -> crate::Result<SequentialBlockFees> {
        self.get_fees(height_range).await
    }

    async fn current_block_height(&self) -> crate::Result<u64> {
        self.fees_provider.current_block_height().await
    }
}

impl<P: FeesProvider> CachingFeesProvider<P> {
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

        SequentialBlockFees::try_from(fees).map_err(|e| crate::Error::Other(e.to_string()))
    }

    async fn shrink_cache(&self) {
        let mut cache = self.cache.write().await;
        while cache.len() > self.cache_limit {
            cache.pop_first();
        }
    }
}

impl<T: Api + Send + Sync> FeesProvider for T {
    async fn fees(&self, height_range: RangeInclusive<u64>) -> crate::Result<SequentialBlockFees> {
        Api::fees(self, height_range).await
    }

    async fn current_block_height(&self) -> crate::Result<u64> {
        Api::current_height(self).await
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

#[cfg(feature = "test-helpers")]
pub mod testing {
    use std::{collections::BTreeMap, ops::RangeInclusive};

    use itertools::Itertools;

    use crate::state_committer::port::l1::{BlockFees, Fees};

    use super::{FeesProvider, SequentialBlockFees};

    #[derive(Debug, Clone, Copy)]
    pub struct ConstantFeesProvider {
        fees: Fees,
    }

    impl ConstantFeesProvider {
        pub fn new(fees: Fees) -> Self {
            Self { fees }
        }
    }

    impl FeesProvider for ConstantFeesProvider {
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

        async fn current_block_height(&self) -> crate::Result<u64> {
            Ok(0)
        }
    }

    #[derive(Debug, Clone)]
    pub struct PreconfiguredFeesProvider {
        fees: BTreeMap<u64, Fees>,
    }

    impl FeesProvider for PreconfiguredFeesProvider {
        async fn current_block_height(&self) -> crate::Result<u64> {
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

    impl PreconfiguredFeesProvider {
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

#[derive(Debug, Clone)]
pub struct FeeAnalytics<P> {
    fees_provider: P,
}
impl<P> FeeAnalytics<P> {
    pub fn new(fees_provider: P) -> Self {
        Self { fees_provider }
    }
}

impl<P: FeesProvider> FeeAnalytics<P> {
    pub async fn calculate_sma(&self, block_range: RangeInclusive<u64>) -> crate::Result<Fees> {
        eprintln!("asking for fees");
        let fees = self.fees_provider.fees(block_range.clone()).await?;
        eprintln!("fees received");

        eprintln!("checking if fees are complete");
        let received_height_range = fees.height_range();
        eprintln!("received height range: {:?}", received_height_range);
        if received_height_range != block_range {
            eprintln!("not equeal {received_height_range:?} != {block_range:?}",);
            return Err(crate::Error::from(format!(
                "fees received from the adapter({received_height_range:?}) don't cover the requested range ({block_range:?})"
            )));
        }

        eprintln!("calculating mean");
        Ok(Self::mean(fees))
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

#[cfg(test)]
mod tests {
    use itertools::Itertools;
    use mockall::{predicate::eq, Sequence};

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
    use std::path::PathBuf;

    #[tokio::test]
    async fn calculates_sma_correctly_for_last_1_block() {
        // given
        let fees_provider = testing::PreconfiguredFeesProvider::new(testing::incrementing_fees(5));
        let fee_analytics = FeeAnalytics::new(fees_provider);

        // when
        let sma = fee_analytics.calculate_sma(4..=4).await.unwrap();

        // then
        assert_eq!(sma.base_fee_per_gas, 5);
        assert_eq!(sma.reward, 5);
        assert_eq!(sma.base_fee_per_blob_gas, 5);
    }

    #[tokio::test]
    async fn calculates_sma_correctly_for_last_5_blocks() {
        // given
        let fees_provider = testing::PreconfiguredFeesProvider::new(testing::incrementing_fees(5));
        let fee_analytics = FeeAnalytics::new(fees_provider);

        // when
        let sma = fee_analytics.calculate_sma(0..=4).await.unwrap();

        // then
        let mean = (5 + 4 + 3 + 2 + 1) / 5;
        assert_eq!(sma.base_fee_per_gas, mean);
        assert_eq!(sma.reward, mean);
        assert_eq!(sma.base_fee_per_blob_gas, mean);
    }

    #[tokio::test]
    async fn errors_out_if_returned_fees_are_not_complete() {
        // given
        let mut fees = testing::incrementing_fees(5);
        fees.remove(&4);
        let fees_provider = testing::PreconfiguredFeesProvider::new(fees);
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
    async fn caching_provider_avoids_duplicate_requests() {
        // given
        let mut mock_provider = MockFeesProvider::new();

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

        let provider = CachingFeesProvider::new(mock_provider, 5);
        let _ = provider.get_fees(0..=4).await.unwrap();

        // when
        let _ = provider.get_fees(0..=4).await.unwrap();

        // then
        // mock validates no extra calls made
    }

    #[tokio::test]
    async fn caching_provider_fetches_only_missing_blocks() {
        // Given: A mock FeesProvider
        let mut mock_provider = MockFeesProvider::new();

        // Expectation: The provider will fetch blocks 3..=5, since 0..=2 are cached
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

        let provider = CachingFeesProvider::new(mock_provider, 5);
        let _ = provider.get_fees(0..=2).await.unwrap();

        // when
        let _ = provider.get_fees(2..=5).await.unwrap();

        // then
        // not called for the overlapping area
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

    #[tokio::test]
    async fn caching_provider_evicts_oldest_blocks() {
        // given
        let mut mock_provider = MockFeesProvider::new();

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

        let provider = CachingFeesProvider::new(mock_provider, 5);
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
        let mut mock_provider = MockFeesProvider::new();

        let cache_limit = 5;

        mock_provider
            .expect_fees()
            .with(eq(0..=9))
            .times(1)
            .returning(|range| Box::pin(async move { Ok(generate_sequential_fees(range)) }));

        let provider = CachingFeesProvider::new(mock_provider, cache_limit);

        // when
        let result = provider.get_fees(0..=9).await.unwrap();

        assert_eq!(result, generate_sequential_fees(0..=9));
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
