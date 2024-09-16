use crate::Result;
use itertools::Itertools;
use ports::{l1::SubmittableFragments, storage::ValidatedRange};

use flate2::{write::GzEncoder, Compression};
use ports::types::NonEmptyVec;
use std::{io::Write, num::NonZeroUsize, ops::Range};
use tracing::info;

#[derive(Debug, Clone, PartialEq)]
pub struct BundleProposal {
    pub fragments: SubmittableFragments,
    pub block_heights: ValidatedRange<u32>,
    pub optimal: bool,
    pub compression_ratio: f64,
}

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait Bundle {
    async fn propose_bundle(&mut self) -> Result<Option<BundleProposal>>;
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
}

impl<L1, Storage> Factory<L1, Storage> {
    pub fn new(
        l1_adapter: L1,
        storage: Storage,
        acceptable_block_range: Range<usize>,
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
            .lowest_unbundled_blocks(self.max_blocks.into())
            .await?;

        // TODO: make compression level configurable
        Ok(Bundler::new(
            self.l1_adapter.clone(),
            blocks,
            self.min_blocks,
            Compressor::default(),
        ))
    }
}

pub struct BestProposal {
    proposal: BundleProposal,
    gas_per_uncompressed_byte: f64,
    uncompressed_data_size: usize,
}

pub struct Bundler<L1> {
    l1_adapter: L1,
    blocks: Vec<ports::storage::FuelBlock>,
    minimum_blocks: NonZeroUsize,
    best_proposal: Option<BestProposal>,
    current_block_count: NonZeroUsize,
    compressor: Compressor,
}

impl<L1> Bundler<L1> {
    pub fn new(
        l1_adapter: L1,
        blocks: Vec<ports::storage::FuelBlock>,
        minimum_blocks: NonZeroUsize,
        compressor: Compressor,
    ) -> Self {
        let blocks = blocks.into_iter().sorted_by_key(|b| b.height).collect();
        Self {
            l1_adapter,
            blocks,
            minimum_blocks,
            best_proposal: None,
            current_block_count: minimum_blocks,
            compressor,
        }
    }

    /// TODO: this should be prevented somehow
    /// Merges the data from the given blocks into a `Vec<u8>`.
    fn merge_block_data(
        &self,
        block_slice: &[ports::storage::FuelBlock],
    ) -> Result<NonEmptyVec<u8>> {
        if block_slice.is_empty() {
            return Err(crate::Error::Other("no blocks to merge".to_string()));
        }

        let bytes = block_slice
            .iter()
            .flat_map(|b| b.data.clone().into_inner())
            .collect_vec();

        Ok(bytes.try_into().expect("Merged data cannot be empty"))
    }

    /// Extracts the block heights from the given blocks as a `ValidatedRange<u32>`.
    fn extract_block_heights(
        &self,
        block_slice: &[ports::storage::FuelBlock],
    ) -> ValidatedRange<u32> {
        let min_height = block_slice
            .first()
            .expect("Block slice cannot be empty")
            .height;
        let max_height = block_slice
            .last()
            .expect("Block slice cannot be empty")
            .height;

        (min_height..max_height.saturating_add(1))
            .try_into()
            .expect("Invalid block height range")
    }

    /// Calculates the gas per uncompressed byte ratio for data.
    fn calculate_gas_per_uncompressed_byte(
        &self,
        gas_estimation: u128,
        uncompressed_data_size: usize,
    ) -> f64 {
        gas_estimation as f64 / uncompressed_data_size as f64
    }

    /// Calculates the compression ratio (uncompressed size / compressed size).
    fn calculate_compression_ratio(&self, uncompressed_size: usize, compressed_size: usize) -> f64 {
        uncompressed_size as f64 / compressed_size as f64
    }

    /// Determines if the current proposal is better based on gas per uncompressed byte and data size.
    fn is_current_proposal_better(&self, gas_per_uncompressed_byte: f64, data_size: usize) -> bool {
        match &self.best_proposal {
            None => true, // No best proposal yet, so the current one is better
            Some(best_proposal) => {
                if gas_per_uncompressed_byte < best_proposal.gas_per_uncompressed_byte {
                    true // Current proposal has a better (lower) gas per uncompressed byte
                } else if gas_per_uncompressed_byte == best_proposal.gas_per_uncompressed_byte {
                    // If the gas per byte is the same, the proposal with more uncompressed data is better
                    data_size > best_proposal.uncompressed_data_size
                } else {
                    false // Current proposal has a worse (higher) gas per uncompressed byte
                }
            }
        }
    }

    /// Updates the best proposal with the current proposal.
    fn update_best_proposal(
        &mut self,
        current_proposal: BundleProposal,
        gas_per_uncompressed_byte: f64,
        uncompressed_data_size: usize,
    ) {
        self.best_proposal = Some(BestProposal {
            proposal: current_proposal,
            gas_per_uncompressed_byte,
            uncompressed_data_size,
        });
    }
}

#[async_trait::async_trait]
impl<L1> Bundle for Bundler<L1>
where
    L1: ports::l1::Api + Send + Sync,
{
    async fn propose_bundle(&mut self) -> Result<Option<BundleProposal>> {
        let min_blocks = self.minimum_blocks;
        if self.blocks.len() < min_blocks.get() {
            info!(
                "Not enough blocks to meet the minimum requirement: {}",
                min_blocks
            );
            return Ok(None);
        }

        let max_blocks = NonZeroUsize::try_from(self.blocks.len())
            .expect("to not be zero since it is not less than the minimum which cannot be zero");

        if self.current_block_count > max_blocks {
            // No more block counts to try; return the best proposal.
            // Mark as optimal if we've tried all possibilities.
            if let Some(mut best_proposal) =
                self.best_proposal.as_ref().map(|bp| bp.proposal.clone())
            {
                best_proposal.optimal = true;
                return Ok(Some(best_proposal));
            } else {
                return Ok(None);
            }
        }

        let block_slice = &self.blocks[..self.current_block_count.get()];

        // Merge block data (uncompressed data)
        let uncompressed_data = self.merge_block_data(block_slice)?;

        // Compress the merged data for better gas usage
        let compressed_data = self.compressor.compress(&uncompressed_data).await?;

        // Calculate compression ratio
        let compression_ratio =
            self.calculate_compression_ratio(uncompressed_data.len(), compressed_data.len());

        // Split into submittable fragments using the compressed data
        let fragments = self
            .l1_adapter
            .split_into_submittable_fragments(&compressed_data)?;

        // Extract block heights
        let block_heights = self.extract_block_heights(block_slice);

        // Calculate gas per uncompressed byte ratio (based on the original, uncompressed data size)
        let uncompressed_data_size = uncompressed_data.len();
        let gas_per_uncompressed_byte = self
            .calculate_gas_per_uncompressed_byte(fragments.gas_estimation, uncompressed_data_size);

        let current_proposal = BundleProposal {
            fragments,
            block_heights,
            optimal: false,
            compression_ratio, // Record the compression ratio
        };

        // Check if the current proposal is better based on gas per uncompressed byte
        if self.is_current_proposal_better(gas_per_uncompressed_byte, uncompressed_data_size) {
            self.update_best_proposal(
                current_proposal,
                gas_per_uncompressed_byte,
                uncompressed_data_size,
            );
        }

        // Prepare for the next iteration
        self.current_block_count = self.current_block_count.saturating_add(1);

        // TODO: refactor double check
        if self.current_block_count > max_blocks {
            // No more block counts to try; return the best proposal.
            // Mark as optimal if we've tried all possibilities.
            if let Some(mut best_proposal) =
                self.best_proposal.as_ref().map(|bp| bp.proposal.clone())
            {
                best_proposal.optimal = true;
                return Ok(Some(best_proposal));
            } else {
                return Ok(None);
            }
        }

        // Return the best proposal so far
        Ok(self.best_proposal.as_ref().map(|bp| bp.proposal.clone()))
    }
}

#[derive(Debug, Clone)]
pub struct Compressor {
    level: Compression,
}

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

    pub async fn compress(&self, data: &NonEmptyVec<u8>) -> Result<NonEmptyVec<u8>> {
        let mut encoder = GzEncoder::new(Vec::new(), self.level);
        encoder
            .write_all(data.inner())
            .map_err(|e| crate::Error::Other(e.to_string()))?;

        encoder
            .finish()
            .map_err(|e| crate::Error::Other(e.to_string()))?
            .try_into()
            .map_err(|_| crate::Error::Other("compression resulted in no data".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use fuel_crypto::SecretKey;
    use itertools::Itertools;
    use ports::{l1::SubmittableFragments, non_empty_vec, types::NonEmptyVec};

    use crate::{
        state_committer::bundler::{Bundle, Bundler, Compressor},
        test_utils::{self, merge_and_compress_blocks},
        Result,
    };

    #[tokio::test]
    async fn gas_optimizing_bundler_works_in_iterations() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = (0..=3)
            .map(|height| test_utils::mocks::fuel::generate_storage_block(height, &secret_key))
            .collect_vec();

        let bundle_of_blocks_0_and_1 = test_utils::merge_and_compress_blocks(&blocks[0..=1]).await;

        let fragment_of_unoptimal_block = test_utils::random_data(100);

        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([(
            bundle_of_blocks_0_and_1.clone(),
            SubmittableFragments {
                fragments: non_empty_vec![fragment_of_unoptimal_block.clone()],
                gas_estimation: 100,
            },
        )]);

        let mut sut = Bundler::new(
            l1_mock,
            blocks,
            2.try_into().unwrap(),
            Compressor::default(),
        );

        // when
        let bundle = sut.propose_bundle().await.unwrap().unwrap();

        // then
        assert_eq!(
            bundle.block_heights,
            (0..2).try_into().unwrap(),
            "Block heights should be in range from 0 to 2"
        );
        assert!(
            !bundle.optimal,
            "Bundle should not be marked as optimal yet"
        );

        Ok(())
    }

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

        // Mock L1 API to estimate gas and return compressed fragments
        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([(
            merge_and_compress_blocks(&blocks).await,
            SubmittableFragments {
                fragments: non_empty_vec![test_utils::random_data(50)], // Compressed size of 50 bytes
                gas_estimation: 100,
            },
        )]);

        let mut bundler = Bundler::new(
            l1_mock,
            blocks,
            2.try_into().unwrap(),
            Compressor::default(),
        );

        // when
        let proposal = bundler.propose_bundle().await.unwrap().unwrap();

        // then
        approx::assert_abs_diff_eq!(proposal.compression_ratio, 55.35, epsilon = 0.01);

        Ok(())
    }

    #[tokio::test]
    async fn adding_a_block_increases_gas_but_improves_compression() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        // Create blocks with repetitive data patterns for high compressibility
        let block_0 = ports::storage::FuelBlock {
            height: 0,
            hash: secret_key.public_key().hash().into(),
            data: NonEmptyVec::try_from(vec![0u8; 1024]).unwrap(), // 1 KB of repetitive 0s
        };
        let block_1 = ports::storage::FuelBlock {
            height: 1,
            hash: secret_key.public_key().hash().into(),
            data: NonEmptyVec::try_from(vec![0u8; 1024]).unwrap(), // 1 KB of repetitive 0s
        };
        let block_2 = ports::storage::FuelBlock {
            height: 2,
            hash: secret_key.public_key().hash().into(),
            data: NonEmptyVec::try_from(vec![1u8; 1024]).unwrap(), // 1 KB of repetitive 1s
        };

        let blocks = vec![block_0.clone(), block_1.clone(), block_2.clone()];

        // Simulate Bundle 1 with only two blocks and lower gas estimation
        let bundle_1_data = merge_and_compress_blocks(&blocks[0..=1]).await;
        let bundle_1_gas = 100;

        // Simulate Bundle 2 with all three blocks and higher gas estimation
        let bundle_2_data = merge_and_compress_blocks(&blocks[0..=2]).await;
        let bundle_2_gas = 150; // Higher gas but better compression

        // Mock L1 API: Bundle 1 and Bundle 2 gas estimates
        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([
            (
                bundle_1_data,
                SubmittableFragments {
                    fragments: non_empty_vec![test_utils::random_data(100)], // Compressed size for 2 blocks
                    gas_estimation: bundle_1_gas,
                },
            ),
            (
                bundle_2_data,
                SubmittableFragments {
                    fragments: non_empty_vec![test_utils::random_data(120)], // Compressed size for 3 blocks
                    gas_estimation: bundle_2_gas,
                },
            ),
        ]);

        let mut bundler = Bundler::new(
            l1_mock,
            blocks.clone(),
            2.try_into().unwrap(),
            Compressor::default(),
        );

        // when
        let best_proposal = loop {
            let proposal = bundler.propose_bundle().await?.unwrap();
            if proposal.optimal {
                break proposal;
            }
        };

        // then
        assert_eq!(best_proposal.block_heights, (0..3).try_into().unwrap());

        approx::assert_abs_diff_eq!(best_proposal.compression_ratio, 80.84, epsilon = 0.01);

        Ok(())
    }

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
        );

        // when
        let proposal = bundler.propose_bundle().await.unwrap();

        // then
        assert!(
            proposal.is_none(),
            "Expected no proposal when blocks are below minimum range"
        );

        Ok(())
    }

    #[tokio::test]
    async fn propose_bundle_with_exact_minimum_blocks() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let block_0 = test_utils::mocks::fuel::generate_storage_block(0, &secret_key);
        let block_1 = test_utils::mocks::fuel::generate_storage_block(1, &secret_key);

        let compressed_data =
            test_utils::merge_and_compress_blocks(&[block_0.clone(), block_1.clone()]).await;
        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([(
            compressed_data.clone(),
            SubmittableFragments {
                fragments: non_empty_vec![test_utils::random_data(50)],
                gas_estimation: 100,
            },
        )]);

        let mut bundler = Bundler::new(
            l1_mock,
            vec![block_0, block_1],
            2.try_into().unwrap(), // Minimum is 2, maximum is 3
            Compressor::default(),
        );

        // when
        let proposal = bundler.propose_bundle().await.unwrap().unwrap();

        // then
        assert_eq!(
            proposal.block_heights,
            (0..2).try_into().unwrap(),
            "Block heights should be in range from 0 to 2"
        );

        Ok(())
    }

    #[tokio::test]
    async fn propose_bundle_with_unsorted_blocks() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = vec![
            test_utils::mocks::fuel::generate_storage_block(2, &secret_key),
            test_utils::mocks::fuel::generate_storage_block(0, &secret_key),
            test_utils::mocks::fuel::generate_storage_block(1, &secret_key),
        ];

        let compressed_data = test_utils::merge_and_compress_blocks(&[
            blocks[1].clone(),
            blocks[2].clone(),
            blocks[0].clone(),
        ])
        .await;
        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([(
            compressed_data.clone(),
            SubmittableFragments {
                fragments: non_empty_vec![test_utils::random_data(70)],
                gas_estimation: 200,
            },
        )]);

        let mut bundler = Bundler::new(
            l1_mock,
            blocks.clone(),
            3.try_into().unwrap(),
            Compressor::default(),
        );

        // when
        let proposal = bundler.propose_bundle().await.unwrap().unwrap();

        // then
        assert!(
            proposal.optimal,
            "Proposal with maximum blocks should be optimal"
        );

        Ok(())
    }
}
