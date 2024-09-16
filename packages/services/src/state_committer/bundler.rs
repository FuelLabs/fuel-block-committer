use crate::Result;
use itertools::Itertools;

use flate2::{write::GzEncoder, Compression};
use ports::{
    l1::{GasPrices, GasUsage},
    types::NonEmptyVec,
};
use std::{io::Write, num::NonZeroUsize, ops::RangeInclusive};
use tracing::info;

#[derive(Debug, Clone, Copy)]
pub struct Compressor {
    level: Compression,
}

#[allow(dead_code)]
pub enum Level {
    Min,
    Level0,
    Level1,
    Level2,
    Level3,
    Level4,
    Level5,
    Level6,
    Level7,
    Level8,
    Level9,
    Level10,
    Max,
}

impl Compressor {
    pub fn new(level: Level) -> Self {
        let level = match level {
            Level::Level0 | Level::Min => 0,
            Level::Level1 => 1,
            Level::Level2 => 2,
            Level::Level3 => 3,
            Level::Level4 => 4,
            Level::Level5 => 5,
            Level::Level6 => 6,
            Level::Level7 => 7,
            Level::Level8 => 8,
            Level::Level9 => 9,
            Level::Level10 | Level::Max => 10,
        };

        Self {
            level: Compression::new(level),
        }
    }

    pub fn default() -> Self {
        Self::new(Level::Level6)
    }

    fn _compress(level: Compression, data: &NonEmptyVec<u8>) -> Result<NonEmptyVec<u8>> {
        let mut encoder = GzEncoder::new(Vec::new(), level);
        encoder
            .write_all(data.inner())
            .map_err(|e| crate::Error::Other(e.to_string()))?;

        encoder
            .finish()
            .map_err(|e| crate::Error::Other(e.to_string()))?
            .try_into()
            .map_err(|_| crate::Error::Other("compression resulted in no data".to_string()))
    }

    pub fn compress_blocking(&self, data: &NonEmptyVec<u8>) -> Result<NonEmptyVec<u8>> {
        Self::_compress(self.level, data)
    }

    pub async fn compress(&self, data: NonEmptyVec<u8>) -> Result<NonEmptyVec<u8>> {
        let level = self.level;
        tokio::task::spawn_blocking(move || Self::_compress(level, &data))
            .await
            .map_err(|e| crate::Error::Other(e.to_string()))?
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BundleProposal {
    pub fragments: NonEmptyVec<NonEmptyVec<u8>>,
    pub block_heights: RangeInclusive<u32>,
    pub optimal: bool,
    pub compression_ratio: f64,
}

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait Bundle {
    /// Attempts to advance the bundler by trying out a new bundle configuration.
    ///
    /// Returns `true` if there are more configurations to process, or `false` otherwise.
    async fn advance(&mut self) -> Result<bool>;

    /// Finalizes the bundling process by selecting the best bundle based on current gas prices.
    ///
    /// Consumes the bundler.
    async fn finish(self) -> Result<Option<BundleProposal>>;
}

#[async_trait::async_trait]
pub trait BundlerFactory {
    type Bundler: Bundle + Send;
    async fn build(&self) -> Result<Self::Bundler>;
}

pub struct Factory<L1, Storage> {
    l1_adapter: L1,
    storage: Storage,
    min_blocks: NonZeroUsize,
    max_blocks: NonZeroUsize,
    compressor: Compressor,
}

impl<L1, Storage> Factory<L1, Storage> {
    pub fn new(
        l1_adapter: L1,
        storage: Storage,
        acceptable_block_range: std::ops::Range<usize>,
        compressor: Compressor,
    ) -> Result<Self> {
        let Some((min, max)) = acceptable_block_range.minmax().into_option() else {
            return Err(crate::Error::Other(
                "acceptable block range must not be empty".to_string(),
            ));
        };

        let min_blocks = NonZeroUsize::new(min).ok_or_else(|| {
            crate::Error::Other("minimum block count must be non-zero".to_string())
        })?;

        let max_blocks = NonZeroUsize::new(max).ok_or_else(|| {
            crate::Error::Other("maximum block count must be non-zero".to_string())
        })?;

        Ok(Self {
            l1_adapter,
            storage,
            min_blocks,
            max_blocks,
            compressor,
        })
    }
}

#[async_trait::async_trait]
impl<L1, Storage> BundlerFactory for Factory<L1, Storage>
where
    Storage: ports::storage::Storage + Send + Sync + 'static,
    L1: ports::l1::Api + Clone + Send + Sync + 'static,
{
    type Bundler = Bundler<L1>;

    async fn build(&self) -> Result<Self::Bundler> {
        let blocks = self
            .storage
            .lowest_unbundled_blocks(self.max_blocks.get())
            .await?;

        Ok(Bundler::new(
            self.l1_adapter.clone(),
            blocks,
            self.min_blocks,
            self.compressor,
            self.max_blocks, // Pass maximum blocks
        ))
    }
}

/// Represents a bundle configuration and its associated gas usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub num_blocks: NonZeroUsize,
    pub uncompressed_data_size: NonZeroUsize,
    pub compressed_data_size: NonZeroUsize,
    pub gas_usage: GasUsage,
}

pub struct Bundler<L1> {
    l1_adapter: L1,
    blocks: Vec<ports::storage::FuelBlock>,
    minimum_blocks: NonZeroUsize,
    maximum_blocks: NonZeroUsize,
    gas_usages: Vec<Proposal>, // Track all proposals
    current_block_count: NonZeroUsize,
    compressor: Compressor,
}

impl<L1> Bundler<L1>
where
    L1: ports::l1::Api + Send + Sync,
{
    pub fn new(
        l1_adapter: L1,
        blocks: Vec<ports::storage::FuelBlock>,
        minimum_blocks: NonZeroUsize,
        compressor: Compressor,
        maximum_blocks: NonZeroUsize,
    ) -> Self {
        let mut blocks = blocks;
        blocks.sort_unstable_by_key(|b| b.height);
        Self {
            l1_adapter,
            blocks,
            minimum_blocks,
            maximum_blocks,
            gas_usages: Vec::new(),
            current_block_count: minimum_blocks,
            compressor,
        }
    }

    /// Selects the best proposal based on the current gas prices.
    fn select_best_proposal(&self, gas_prices: &GasPrices) -> Result<&Proposal> {
        self.gas_usages
            .iter()
            .min_by(|a, b| {
                let fee_a = Self::calculate_fee_per_byte(
                    &a.gas_usage,
                    &a.uncompressed_data_size,
                    gas_prices,
                );
                let fee_b = Self::calculate_fee_per_byte(
                    &b.gas_usage,
                    &b.uncompressed_data_size,
                    gas_prices,
                );
                fee_a
                    .partial_cmp(&fee_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or_else(|| crate::Error::Other("No proposals available".to_string()))
    }

    /// Calculates the block heights range based on the number of blocks.
    fn calculate_block_heights(&self, num_blocks: NonZeroUsize) -> Result<RangeInclusive<u32>> {
        if num_blocks.get() > self.blocks.len() {
            return Err(crate::Error::Other(
                "Invalid number of blocks for proposal".to_string(),
            ));
        }

        let first_block = &self.blocks[0];
        let last_block = &self.blocks[num_blocks.get().saturating_sub(1)];

        Ok(first_block.height..=last_block.height)
    }

    /// Recompresses the data for the best bundle configuration.
    async fn compress_first_n_blocks(&self, num_blocks: NonZeroUsize) -> Result<NonEmptyVec<u8>> {
        let blocks = self
            .blocks
            .iter()
            .take(num_blocks.get())
            .cloned()
            .collect::<Vec<_>>();
        let blocks = NonEmptyVec::try_from(blocks).expect("Should have at least one block");

        let uncompressed_data = self.merge_block_data(blocks);
        self.compressor.compress(uncompressed_data).await
    }

    /// Calculates the fee per uncompressed byte.
    fn calculate_fee_per_byte(
        gas_usage: &GasUsage,
        uncompressed_size: &NonZeroUsize,
        gas_prices: &GasPrices,
    ) -> f64 {
        let storage_fee = gas_usage.storage.saturating_mul(gas_prices.storage);
        let normal_fee = gas_usage.normal.saturating_mul(gas_prices.normal);

        let total_fee = storage_fee.saturating_add(normal_fee);

        total_fee as f64 / uncompressed_size.get() as f64
    }

    /// Calculates the compression ratio (uncompressed size / compressed size).
    fn calculate_compression_ratio(
        &self,
        uncompressed_size: NonZeroUsize,
        compressed_size: NonZeroUsize,
    ) -> f64 {
        uncompressed_size.get() as f64 / compressed_size.get() as f64
    }

    /// Merges the data from multiple blocks into a single `NonEmptyVec<u8>`.
    fn merge_block_data(&self, blocks: NonEmptyVec<ports::storage::FuelBlock>) -> NonEmptyVec<u8> {
        let bytes = blocks.into_iter().flat_map(|b| b.data).collect_vec();
        bytes.try_into().expect("Cannot be empty")
    }

    /// Retrieves the next bundle configuration.
    fn blocks_for_new_proposal(&self) -> NonEmptyVec<ports::storage::FuelBlock> {
        NonEmptyVec::try_from(
            self.blocks
                .iter()
                .take(self.current_block_count.get())
                .cloned()
                .collect::<Vec<_>>(),
        )
        .expect("should never be empty")
    }

    /// Creates a proposal for the given bundle configuration.
    async fn create_proposal(
        &self,
        bundle_blocks: NonEmptyVec<ports::storage::FuelBlock>,
    ) -> Result<Proposal> {
        let uncompressed_data = self.merge_block_data(bundle_blocks.clone());
        let uncompressed_data_size = uncompressed_data.len();

        // Compress the data to get compressed_size
        let compressed_data = self.compressor.compress(uncompressed_data.clone()).await?;
        let compressed_size = compressed_data.len();

        // Estimate gas usage based on compressed data
        let gas_usage = self.l1_adapter.gas_usage_to_store_data(&compressed_data);

        Ok(Proposal {
            num_blocks: self.current_block_count,
            uncompressed_data_size,
            compressed_data_size: compressed_size,
            gas_usage,
        })
    }
}

#[async_trait::async_trait]
impl<L1> Bundle for Bundler<L1>
where
    L1: ports::l1::Api + Send + Sync,
{
    /// Advances the bundler by trying the next bundle configuration.
    ///
    /// Returns `true` if there are more configurations to process, or `false` otherwise.
    async fn advance(&mut self) -> Result<bool> {
        if self.blocks.len() < self.minimum_blocks.get() {
            info!(
                "Not enough blocks to meet the minimum requirement: {}",
                self.minimum_blocks
            );
            return Ok(false);
        }

        if self.current_block_count.get() > self.maximum_blocks.get() {
            // Reached the maximum bundle size
            return Ok(false);
        }

        let bundle_blocks = self.blocks_for_new_proposal();

        let proposal = self.create_proposal(bundle_blocks).await?;

        self.gas_usages.push(proposal);

        self.current_block_count = self.current_block_count.saturating_add(1);

        // Return whether there are more configurations to process
        Ok(self.current_block_count.get() <= self.maximum_blocks.get())
    }

    /// Finalizes the bundling process by selecting the best bundle based on current gas prices.
    ///
    /// Consumes the bundler.
    async fn finish(self) -> Result<Option<BundleProposal>> {
        if self.gas_usages.is_empty() {
            return Ok(None);
        }

        // Fetch current gas prices
        let gas_prices = self.l1_adapter.gas_prices().await?;

        // Select the best proposal based on current gas prices
        let best_proposal = self.select_best_proposal(&gas_prices)?;

        // Determine the block height range based on the number of blocks in the best proposal
        let block_heights = self.calculate_block_heights(best_proposal.num_blocks)?;

        // Recompress the best bundle's data
        let compressed_data = self
            .compress_first_n_blocks(best_proposal.num_blocks)
            .await?;

        // Split into submittable fragments
        let fragments = self
            .l1_adapter
            .split_into_submittable_fragments(&compressed_data)?;

        // Calculate compression ratio
        let compression_ratio = self.calculate_compression_ratio(
            best_proposal.uncompressed_data_size,
            compressed_data.len(),
        );

        // Determine if all configurations have been tried
        let all_proposals_tried = self.current_block_count.get() > self.maximum_blocks.get();

        Ok(Some(BundleProposal {
            fragments,
            block_heights,
            optimal: all_proposals_tried,
            compression_ratio,
        }))
    }
}

#[cfg(test)]
mod tests {
    use fuel_crypto::SecretKey;
    use itertools::Itertools;
    use ports::{l1::GasUsage, non_empty_vec, types::NonEmptyVec};

    use crate::{
        state_committer::bundler::{Bundle, BundleProposal, Bundler, Compressor},
        test_utils::{self, merge_and_compress_blocks},
        Result,
    };

    /// Test that the bundler correctly iterates through different bundle configurations
    /// and selects the optimal one based on gas efficiency.
    #[tokio::test]
    async fn gas_optimizing_bundler_works_in_iterations() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = (0..=3)
            .map(|height| test_utils::mocks::fuel::generate_storage_block(height, &secret_key))
            .collect_vec();

        // Simulate different compressed data and gas usage for bundle sizes 2, 3, 4
        let bundle_2_data = test_utils::merge_and_compress_blocks(&blocks[0..=1]).await;
        let bundle_2_gas = GasUsage {
            storage: 100,
            normal: 50,
        }; // Example gas usage for 2 blocks

        let bundle_3_data = test_utils::merge_and_compress_blocks(&blocks[0..=2]).await;
        let bundle_3_gas = GasUsage {
            storage: 150,
            normal: 75,
        }; // Example gas usage for 3 blocks

        let bundle_4_data = test_utils::merge_and_compress_blocks(&blocks[0..=3]).await;
        let bundle_4_gas = GasUsage {
            storage: 200,
            normal: 100,
        }; // Example gas usage for 4 blocks

        // Mock L1 API to respond with compressed data and gas usage for each bundle size
        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([
            (
                bundle_2_data.clone(),
                SubmittableFragments {
                    fragments: non_empty_vec![test_utils::random_data(100)], // Compressed size for 2 blocks
                    gas_estimation: bundle_2_gas,
                },
            ),
            (
                bundle_3_data.clone(),
                SubmittableFragments {
                    fragments: non_empty_vec![test_utils::random_data(150)], // Compressed size for 3 blocks
                    gas_estimation: bundle_3_gas,
                },
            ),
            (
                bundle_4_data.clone(),
                SubmittableFragments {
                    fragments: non_empty_vec![test_utils::random_data(200)], // Compressed size for 4 blocks
                    gas_estimation: bundle_4_gas,
                },
            ),
        ]);

        let mut bundler = Bundler::new(
            l1_mock,
            blocks,
            2.try_into().unwrap(),
            Compressor::default(),
            4.try_into().unwrap(), // Set maximum blocks
        );

        // when
        let mut has_more = true;
        while has_more {
            has_more = bundler.advance().await?;
        }

        let bundle = bundler.finish().await.unwrap();

        // then
        // All bundles have the same fee per byte, so the bundler selects the first one (2 blocks)
        assert_eq!(
            bundle.block_heights,
            0..=1,
            "Block heights should be in range from 0 to 1"
        );
        assert!(bundle.optimal, "Bundle should be marked as optimal");

        // Calculate compression ratio: uncompressed_size / compressed_size
        // For bundle 2: 4096 / 100 = 40.96
        assert_eq!(
            bundle.compression_ratio, 40.96,
            "Compression ratio should be correctly calculated"
        );

        Ok(())
    }

    /// Test that the bundler correctly calculates gas usage and compression ratio.
    #[tokio::test]
    async fn returns_gas_used_and_compression_ratio() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        // Create blocks with repetitive data patterns to ensure compressibility
        let block_0 = ports::storage::FuelBlock {
            height: 0,
            hash: secret_key.public_key().hash().into(),
            data: NonEmptyVec::try_from(vec![0u8; 1024]).unwrap(), // 1 KB of repetitive 0s
        };
        let block_1 = ports::storage::FuelBlock {
            height: 1,
            hash: secret_key.public_key().hash().into(),
            data: NonEmptyVec::try_from(vec![1u8; 1024]).unwrap(), // 1 KB of repetitive 1s
        };

        let blocks = vec![block_0.clone(), block_1.clone()];

        // Bundle size 2
        let bundle_2_data = test_utils::merge_and_compress_blocks(&blocks[0..=1]).await;
        let bundle_2_gas = GasUsage {
            storage: 50,
            normal: 50,
        }; // Example gas usage for 2 blocks

        // Mock L1 API to estimate gas and return compressed fragments for bundle size 2
        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([(
            bundle_2_data.clone(),
            SubmittableFragments {
                fragments: non_empty_vec![test_utils::random_data(50)], // Compressed size of 50 bytes
                gas_estimation: bundle_2_gas,
            },
        )]);

        let mut bundler = Bundler::new(
            l1_mock,
            blocks,
            2.try_into().unwrap(),
            Compressor::default(),
            2.try_into().unwrap(), // Set maximum blocks to 2
        );

        // when
        let mut has_more = true;
        while has_more {
            has_more = bundler.advance().await?;
        }

        let proposal = bundler.finish().await.unwrap();

        // then
        // Compression ratio: 2048 / 50 = 40.96
        assert_eq!(
            proposal.block_heights,
            0..=1,
            "Block heights should be in range from 0 to 1"
        );
        assert_eq!(
            proposal.compression_ratio, 40.96,
            "Compression ratio should be correctly calculated"
        );
        assert!(proposal.optimal, "Bundle should be marked as optimal");

        Ok(())
    }

    /// Test that adding a block increases gas but improves compression ratio.
    #[tokio::test]
    async fn adding_a_block_increases_gas_but_improves_compression() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        // Create blocks with repetitive data patterns for high compressibility
        let block_0 = ports::storage::FuelBlock {
            height: 0,
            hash: secret_key.public_key().hash().into(),
            data: NonEmptyVec::try_from(vec![0u8; 2048]).unwrap(), // 2 KB of repetitive 0s
        };
        let block_1 = ports::storage::FuelBlock {
            height: 1,
            hash: secret_key.public_key().hash().into(),
            data: NonEmptyVec::try_from(vec![0u8; 2048]).unwrap(), // 2 KB of repetitive 0s
        };
        let block_2 = ports::storage::FuelBlock {
            height: 2,
            hash: secret_key.public_key().hash().into(),
            data: NonEmptyVec::try_from(vec![1u8; 2048]).unwrap(), // 2 KB of repetitive 1s
        };

        let blocks = vec![block_0.clone(), block_1.clone(), block_2.clone()];

        // Simulate different compressed data and gas usage for bundle sizes 2, 3
        let bundle_2_data = test_utils::merge_and_compress_blocks(&blocks[0..=1]).await;
        let bundle_2_gas = GasUsage {
            storage: 100,
            normal: 50,
        }; // Example gas usage for 2 blocks

        let bundle_3_data = test_utils::merge_and_compress_blocks(&blocks[0..=2]).await;
        let bundle_3_gas = GasUsage {
            storage: 130,
            normal: 70,
        }; // Example gas usage for 3 blocks

        // Mock L1 API to respond with compressed data and gas usage for each bundle size
        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([
            (
                bundle_2_data.clone(),
                SubmittableFragments {
                    fragments: non_empty_vec![test_utils::random_data(100)], // Compressed size for 2 blocks
                    gas_estimation: bundle_2_gas,
                },
            ),
            (
                bundle_3_data.clone(),
                SubmittableFragments {
                    fragments: non_empty_vec![test_utils::random_data(150)], // Compressed size for 3 blocks
                    gas_estimation: bundle_3_gas,
                },
            ),
        ]);

        let mut bundler = Bundler::new(
            l1_mock,
            blocks.clone(),
            2.try_into().unwrap(),
            Compressor::default(),
            3.try_into().unwrap(), // Set maximum blocks to 3
        );

        // when
        while bundler.advance().await? {}

        let best_proposal = bundler.finish().await.unwrap();

        // then
        // Calculate fee per byte for each bundle:
        // Bundle 2: (100 + 50) / 4096 = 0.036621
        // Bundle 3: (130 + 70) / 6144 = 0.036621
        // Both have the same fee per byte; the bundler should select the first one (2 blocks)
        assert_eq!(best_proposal.block_heights, 0..=1);
        assert!(best_proposal.optimal, "Bundle should be marked as optimal");

        // Compression ratio: 4096 / 100 = 40.96
        assert_eq!(
            best_proposal.compression_ratio, 40.96,
            "Compression ratio should be correctly calculated"
        );

        Ok(())
    }

    /// Test that the bundler returns `None` when there are insufficient blocks to meet the minimum requirement.
    #[tokio::test]
    async fn propose_bundle_with_insufficient_blocks_returns_none() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let block = test_utils::mocks::fuel::generate_storage_block(0, &secret_key);

        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([]);

        let mut bundler = Bundler::new(
            l1_mock,
            vec![block],
            2.try_into().unwrap(), // Minimum required blocks is 2
            Compressor::default(),
            3.try_into().unwrap(), // Set maximum blocks to 3
        );

        // when
        let has_more = bundler.advance().await?;

        // Attempt to finish early
        let proposal = if has_more {
            bundler.finish().await?
        } else {
            // No more configurations to process, attempt to finish
            bundler.finish().await?
        };

        // then
        assert!(
            proposal.is_none(),
            "Expected no proposal when blocks are below minimum range"
        );

        Ok(())
    }

    /// Test that the bundler correctly handles proposals with exactly the minimum number of blocks.
    #[tokio::test]
    async fn propose_bundle_with_exact_minimum_blocks() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let block_0 = test_utils::mocks::fuel::generate_storage_block(0, &secret_key);
        let block_1 = test_utils::mocks::fuel::generate_storage_block(1, &secret_key);

        // Simulate bundle size 2
        let bundle_2_data =
            test_utils::merge_and_compress_blocks(&[block_0.clone(), block_1.clone()]).await;
        let bundle_2_gas = GasUsage {
            storage: 50,
            normal: 50,
        }; // Example gas usage for 2 blocks

        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([(
            bundle_2_data.clone(),
            SubmittableFragments {
                fragments: non_empty_vec![test_utils::random_data(50)],
                gas_estimation: bundle_2_gas,
            },
        )]);

        let mut bundler = Bundler::new(
            l1_mock,
            vec![block_0, block_1],
            2.try_into().unwrap(), // Minimum is 2
            Compressor::default(),
            3.try_into().unwrap(), // Set maximum blocks to 3
        );

        // when
        let mut has_more = true;
        while has_more {
            has_more = bundler.advance().await?;
        }

        let proposal = bundler.finish().await.unwrap();

        // then
        assert_eq!(
            proposal.block_heights,
            0..=1,
            "Block heights should be in expected range"
        );
        assert!(proposal.optimal, "Bundle should be marked as optimal");

        // Compression ratio: 2048 / 50 = 40.96
        assert_eq!(
            proposal.compression_ratio, 40.96,
            "Compression ratio should be correctly calculated"
        );

        Ok(())
    }

    /// Test that the bundler correctly handles unsorted blocks by sorting them internally.
    #[tokio::test]
    async fn propose_bundle_with_unsorted_blocks() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = vec![
            test_utils::mocks::fuel::generate_storage_block(2, &secret_key),
            test_utils::mocks::fuel::generate_storage_block(0, &secret_key),
            test_utils::mocks::fuel::generate_storage_block(1, &secret_key),
        ];

        // Simulate compressed data and gas usage for bundle sizes 3
        let bundle_3_data = test_utils::merge_and_compress_blocks(&[
            blocks[1].clone(), // Block 0
            blocks[2].clone(), // Block 1
            blocks[0].clone(), // Block 2
        ])
        .await;
        let bundle_3_gas = GasUsage {
            storage: 200,
            normal: 0,
        }; // Example gas usage for 3 blocks

        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([(
            bundle_3_data.clone(),
            SubmittableFragments {
                fragments: non_empty_vec![test_utils::random_data(70)],
                gas_estimation: bundle_3_gas,
            },
        )]);

        let mut bundler = Bundler::new(
            l1_mock,
            blocks.clone(),
            3.try_into().unwrap(),
            Compressor::default(),
            3.try_into().unwrap(), // Set maximum blocks to 3
        );

        // when
        let mut has_more = true;
        while has_more {
            has_more = bundler.advance().await?;
        }

        let proposal = bundler.finish().await.unwrap();

        // then
        assert!(
            proposal.optimal,
            "Proposal with maximum blocks should be optimal"
        );

        // Compression ratio: 6144 / 70 = 87.77
        assert_eq!(
            proposal.compression_ratio, 87.77,
            "Compression ratio should be correctly calculated"
        );

        Ok(())
    }

    /// Test that bundles with more compressed data use less gas per byte.
    #[tokio::test]
    async fn bundle_with_more_compressed_data_uses_less_gas_per_byte() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        // Create highly compressible blocks
        let block_0 = ports::storage::FuelBlock {
            height: 0,
            hash: secret_key.public_key().hash().into(),
            data: NonEmptyVec::try_from(vec![0u8; 4096]).unwrap(), // 4 KB of repetitive 0s
        };
        let block_1 = ports::storage::FuelBlock {
            height: 1,
            hash: secret_key.public_key().hash().into(),
            data: NonEmptyVec::try_from(vec![0u8; 4096]).unwrap(), // 4 KB of repetitive 0s
        };
        let block_2 = ports::storage::FuelBlock {
            height: 2,
            hash: secret_key.public_key().hash().into(),
            data: NonEmptyVec::try_from(vec![1u8; 4096]).unwrap(), // 4 KB of repetitive 1s
        };

        let blocks = vec![block_0.clone(), block_1.clone(), block_2.clone()];

        // Bundle size 2: highly compressible (all 0s)
        let bundle_2_data = test_utils::merge_and_compress_blocks(&blocks[0..=1]).await;
        let bundle_2_gas = GasUsage {
            storage: 80,
            normal: 20,
        }; // Lower gas due to better compression

        // Bundle size 3: less compressible (includes 1s)
        let bundle_3_data = test_utils::merge_and_compress_blocks(&blocks[0..=2]).await;
        let bundle_3_gas = GasUsage {
            storage: 150,
            normal: 50,
        }; // Higher gas due to less compression

        // Mock L1 API to respond with compressed data and gas usage for each bundle size
        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([
            (
                bundle_2_data.clone(),
                SubmittableFragments {
                    fragments: non_empty_vec![test_utils::random_data(80)], // Compressed size for 2 blocks
                    gas_estimation: bundle_2_gas,
                },
            ),
            (
                bundle_3_data.clone(),
                SubmittableFragments {
                    fragments: non_empty_vec![test_utils::random_data(100)], // Compressed size for 3 blocks
                    gas_estimation: bundle_3_gas,
                },
            ),
        ]);

        let mut bundler = Bundler::new(
            l1_mock,
            blocks.clone(),
            2.try_into().unwrap(),
            Compressor::default(),
            3.try_into().unwrap(), // Set maximum blocks to 3
        );

        // when
        while bundler.advance().await? {}

        let best_proposal = bundler.finish().await.unwrap();

        // then
        // Calculate fee per byte for each bundle:
        // Bundle 2: (80 + 20) / 8192 = 100 / 8192 ≈ 0.012207
        // Bundle 3: (150 + 50) / 12288 = 200 / 12288 ≈ 0.016259
        // Bundle 2 has a lower fee per byte and should be selected

        assert_eq!(best_proposal.block_heights, 0..=1);
        assert!(best_proposal.optimal, "Bundle should be marked as optimal");

        // Compression ratio: 8192 / 80 = 102.4
        assert_eq!(
            best_proposal.compression_ratio, 102.4,
            "Compression ratio should be correctly calculated"
        );

        Ok(())
    }
}
