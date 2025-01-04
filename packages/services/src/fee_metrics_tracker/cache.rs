use std::{collections::BTreeMap, ops::RangeInclusive, sync::Arc};

use tokio::sync::Mutex;

use super::port::l1::{Api, BlockFees, Fees, SequentialBlockFees};
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct CachingApi<P> {
    fees_provider: P,
    // preferred over RwLock because of simplicity
    cache: Arc<Mutex<BTreeMap<u64, Fees>>>,
    cache_limit: usize,
}

impl<P> CachingApi<P> {
    pub const fn inner(&self) -> &P {
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

    pub async fn get_fees(&self, height_range: RangeInclusive<u64>) -> Result<SequentialBlockFees> {
        let mut cache_guard = self.cache.lock().await;
        let mut fees = Self::read_cached_fees(&cache_guard, height_range.clone());
        let missing_fees = self.download_missing_fees(&fees, height_range).await?;

        self.update_cache(&mut cache_guard, missing_fees.clone());
        drop(cache_guard);

        fees.extend(missing_fees);
        fees.sort_by_key(|f| f.height);

        SequentialBlockFees::try_from(fees).map_err(|e| Error::Other(e.to_string()))
    }

    fn read_cached_fees(
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

    fn update_cache(&self, cache: &mut BTreeMap<u64, Fees>, fees: Vec<BlockFees>) {
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
    use std::{ops::RangeInclusive, sync::Arc};

    use mockall::{predicate::eq, Sequence};
    use tokio::sync::Barrier;

    use crate::fee_metrics_tracker::{
        cache::CachingApi,
        port::l1::{BlockFees, Fees, MockApi, SequentialBlockFees},
    };

    #[tokio::test]
    async fn avoids_duplicate_requests() {
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
    async fn fetches_only_missing_blocks() {
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
    async fn evicts_oldest_blocks() {
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
    async fn handles_request_larger_than_cache() {
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

    #[tokio::test]
    async fn handles_concurrent_requests() {
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
        let handle1 = tokio::spawn(async move { provider_clone.get_fees(0..=4).await.unwrap() });

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
    async fn import_and_export() {
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
    async fn handles_single_element_range() {
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
    async fn handles_overlapping_ranges() {
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
    async fn export_after_import() {
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
    async fn updates_cache_correctly() {
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
