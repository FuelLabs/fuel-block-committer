use crate::Result;
use itertools::Itertools;

use flate2::{write::GzEncoder, Compression};
use ports::{storage::SequentialFuelBlocks, types::NonEmptyVec};
use std::{io::Write, num::NonZeroUsize, ops::RangeInclusive, str::FromStr};

#[derive(Debug, Clone, Copy)]
struct Compressor {
    compression: Option<Compression>,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum CompressionLevel {
    Disabled,
    Min,
    Level1,
    Level2,
    Level3,
    Level4,
    Level5,
    Level6,
    Level7,
    Level8,
    Level9,
    Max,
}

impl<'a> serde::Deserialize<'a> for CompressionLevel {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        let as_string = String::deserialize(deserializer)?;

        CompressionLevel::from_str(&as_string)
            .map_err(|e| serde::de::Error::custom(format!("Invalid compression level: {e}")))
    }
}

impl FromStr for CompressionLevel {
    type Err = crate::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "min" => Ok(Self::Min),
            "level1" => Ok(Self::Level1),
            "level2" => Ok(Self::Level2),
            "level3" => Ok(Self::Level3),
            "level4" => Ok(Self::Level4),
            "level5" => Ok(Self::Level5),
            "level6" => Ok(Self::Level6),
            "level7" => Ok(Self::Level7),
            "level8" => Ok(Self::Level8),
            "level9" => Ok(Self::Level9),
            "max" => Ok(Self::Max),
            _ => Err(crate::Error::Other(format!(
                "Invalid compression level: {s}"
            ))),
        }
    }
}

impl CompressionLevel {
    pub fn levels() -> Vec<Self> {
        vec![
            Self::Disabled,
            Self::Min,
            Self::Level1,
            Self::Level2,
            Self::Level3,
            Self::Level4,
            Self::Level5,
            Self::Level6,
            Self::Level7,
            Self::Level8,
            Self::Level9,
            Self::Max,
        ]
    }
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new(CompressionLevel::Level6)
    }
}

impl Compressor {
    #[cfg(test)]
    pub fn no_compression() -> Self {
        Self::new(CompressionLevel::Disabled)
    }

    pub fn new(level: CompressionLevel) -> Self {
        let level = match level {
            CompressionLevel::Disabled => None,
            CompressionLevel::Min => Some(0),
            CompressionLevel::Level1 => Some(1),
            CompressionLevel::Level2 => Some(2),
            CompressionLevel::Level3 => Some(3),
            CompressionLevel::Level4 => Some(4),
            CompressionLevel::Level5 => Some(5),
            CompressionLevel::Level6 => Some(6),
            CompressionLevel::Level7 => Some(7),
            CompressionLevel::Level8 => Some(8),
            CompressionLevel::Level9 => Some(9),
            CompressionLevel::Max => Some(10),
        };

        Self {
            compression: level.map(Compression::new),
        }
    }

    fn _compress(
        compression: Option<Compression>,
        data: &NonEmptyVec<u8>,
    ) -> Result<NonEmptyVec<u8>> {
        let Some(level) = compression else {
            return Ok(data.clone());
        };

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

    #[cfg(test)]
    pub fn compress_blocking(&self, data: &NonEmptyVec<u8>) -> Result<NonEmptyVec<u8>> {
        Self::_compress(self.compression, data)
    }

    pub async fn compress(&self, data: NonEmptyVec<u8>) -> Result<NonEmptyVec<u8>> {
        let level = self.compression;
        tokio::task::spawn_blocking(move || Self::_compress(level, &data))
            .await
            .map_err(|e| crate::Error::Other(e.to_string()))?
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BundleProposal {
    pub fragments: NonEmptyVec<NonEmptyVec<u8>>,
    pub block_heights: RangeInclusive<u32>,
    pub known_to_be_optimal: bool,
    pub compression_ratio: f64,
    pub gas_usage: u64,
}

#[trait_variant::make(Send)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Bundle {
    /// Attempts to advance the bundler by trying out a new bundle configuration.
    ///
    /// Returns `true` if there are more configurations to process, or `false` otherwise.
    async fn advance(&mut self) -> Result<bool>;

    /// Finalizes the bundling process by selecting the best bundle based on current gas prices.
    ///
    /// Consumes the bundler.
    async fn finish(self) -> Result<BundleProposal>;
}

#[trait_variant::make(Send)]
pub trait BundlerFactory {
    type Bundler: Bundle + Send + Sync;
    async fn build(&self, blocks: SequentialFuelBlocks) -> Self::Bundler;
}

pub struct Factory<GasCalculator> {
    gas_calc: GasCalculator,
    compression_level: CompressionLevel,
}

impl<L1> Factory<L1> {
    pub fn new(gas_calc: L1, compression_level: CompressionLevel) -> Self {
        Self {
            gas_calc,
            compression_level,
        }
    }
}

impl<GasCalculator> BundlerFactory for Factory<GasCalculator>
where
    GasCalculator: ports::l1::FragmentEncoder + Clone + Send + Sync + 'static,
{
    type Bundler = Bundler<GasCalculator>;

    async fn build(&self, blocks: SequentialFuelBlocks) -> Self::Bundler {
        Bundler::new(
            self.gas_calc.clone(),
            blocks,
            Compressor::new(self.compression_level),
        )
    }
}

/// Represents a bundle configuration and its associated gas usage.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Proposal {
    num_blocks: NonZeroUsize,
    uncompressed_data_size: NonZeroUsize,
    compressed_data_size: NonZeroUsize,
    gas_usage: u64,
}

#[derive(Debug, Clone)]
pub struct Bundler<FragmentEncoder> {
    fragment_encoder: FragmentEncoder,
    blocks: NonEmptyVec<ports::storage::FuelBlock>,
    gas_usages: Vec<Proposal>,
    current_block_count: NonZeroUsize,
    attempts_exhausted: bool,
    compressor: Compressor,
}

impl<T> Bundler<T>
where
    T: ports::l1::FragmentEncoder + Send + Sync,
{
    fn new(cost_calculator: T, blocks: SequentialFuelBlocks, compressor: Compressor) -> Self {
        Self {
            fragment_encoder: cost_calculator,
            current_block_count: blocks.len(),
            blocks: blocks.into_inner(),
            gas_usages: Vec::new(),
            compressor,
            attempts_exhausted: false,
        }
    }

    /// Selects the best proposal based on the current gas prices.
    fn select_best_proposal(&self) -> Result<&Proposal> {
        self.gas_usages
            .iter()
            .min_by(|a, b| {
                let fee_a = a.gas_usage as f64 / a.uncompressed_data_size.get() as f64;
                let fee_b = b.gas_usage as f64 / b.uncompressed_data_size.get() as f64;

                fee_a
                    .partial_cmp(&fee_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or_else(|| crate::Error::Other("No proposals available".to_string()))
    }

    /// Calculates the block heights range based on the number of blocks.
    fn calculate_block_heights(&self, num_blocks: NonZeroUsize) -> Result<RangeInclusive<u32>> {
        if num_blocks > self.blocks.len() {
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
        // TODO: segfault graceful shutdown trigger needed here
        let blocks = self
            .blocks
            .inner()
            .iter()
            .take(num_blocks.get())
            .cloned()
            .collect::<Vec<_>>();
        let blocks = NonEmptyVec::try_from(blocks).expect("Should have at least one block");

        let uncompressed_data = self.merge_block_data(blocks);
        self.compressor.compress(uncompressed_data).await
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
        let bytes = blocks
            .into_inner()
            .into_iter()
            .flat_map(|b| b.data.into_inner())
            .collect_vec();
        bytes.try_into().expect("Cannot be empty")
    }

    /// Retrieves the next bundle configuration.
    fn blocks_for_new_proposal(&self) -> NonEmptyVec<ports::storage::FuelBlock> {
        NonEmptyVec::try_from(
            self.blocks
                .inner()
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
        let gas_usage = self.fragment_encoder.gas_usage(compressed_data.len());

        Ok(Proposal {
            num_blocks: self.current_block_count,
            uncompressed_data_size,
            compressed_data_size: compressed_size,
            gas_usage,
        })
    }
}

impl<T> Bundle for Bundler<T>
where
    T: ports::l1::FragmentEncoder + Send + Sync,
{
    /// Advances the bundler by trying the next bundle configuration.
    ///
    /// Returns `true` if there are more configurations to process, or `false` otherwise.
    async fn advance(&mut self) -> Result<bool> {
        let bundle_blocks = self.blocks_for_new_proposal();

        let proposal = self.create_proposal(bundle_blocks).await?;

        self.gas_usages.push(proposal);

        let more_attempts = if self.current_block_count.get() > 1 {
            let new_block_count = self.current_block_count.get().saturating_sub(1);

            self.current_block_count =
                NonZeroUsize::try_from(new_block_count).expect("greater than 0");

            true
        } else {
            false
        };

        self.attempts_exhausted = !more_attempts;

        // Return whether there are more configurations to process
        Ok(more_attempts)
    }

    /// Finalizes the bundling process by selecting the best bundle based on current gas prices.
    ///
    /// Consumes the bundler.
    async fn finish(mut self) -> Result<BundleProposal> {
        if self.gas_usages.is_empty() {
            self.advance().await?;
        }

        // Select the best proposal based on current gas prices
        let best_proposal = self.select_best_proposal()?;

        // Determine the block height range based on the number of blocks in the best proposal
        let block_heights = self.calculate_block_heights(best_proposal.num_blocks)?;

        // Recompress the best bundle's data
        let compressed_data = self
            .compress_first_n_blocks(best_proposal.num_blocks)
            .await?;

        // Calculate compression ratio
        let compression_ratio = self.calculate_compression_ratio(
            best_proposal.uncompressed_data_size,
            compressed_data.len(),
        );

        let fragments = self.fragment_encoder.encode(compressed_data)?;

        Ok(BundleProposal {
            fragments,
            block_heights,
            known_to_be_optimal: self.attempts_exhausted,
            compression_ratio,
            gas_usage: best_proposal.gas_usage,
        })
    }
}

#[cfg(test)]
mod tests {

    use eth::Eip4844BlobEncoder;

    use fuel_crypto::SecretKey;
    use ports::l1::FragmentEncoder;
    use ports::non_empty_vec;

    use crate::test_utils::mocks::fuel::{generate_storage_block, generate_storage_block_sequence};

    use super::*;

    #[test]
    fn can_disable_compression() {
        // given
        let compressor = Compressor::new(CompressionLevel::Disabled);
        let data = non_empty_vec!(1, 2, 3);

        // when
        let compressed = compressor.compress_blocking(&data).unwrap();

        // then
        assert_eq!(data, compressed);
    }

    #[test]
    fn all_compression_levels_work() {
        let data = non_empty_vec!(1, 2, 3);
        for level in CompressionLevel::levels() {
            let compressor = Compressor::new(level);
            compressor.compress_blocking(&data).unwrap();
        }
    }

    #[tokio::test]
    async fn finishing_will_advance_if_not_called_at_least_once() {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = generate_storage_block_sequence(0..=0, &secret_key, 10, 100);

        let bundler = Bundler::new(
            Eip4844BlobEncoder,
            blocks.clone(),
            Compressor::no_compression(),
        );

        // when
        let bundle = bundler.finish().await.unwrap();

        // then
        let expected_fragments = Eip4844BlobEncoder.encode(blocks[0].data.clone()).unwrap();
        assert!(bundle.known_to_be_optimal);
        assert_eq!(bundle.block_heights, 0..=0);
        assert_eq!(bundle.fragments, expected_fragments);
    }

    #[tokio::test]
    async fn will_provide_a_suboptimal_bundle_if_not_advanced_enough() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        let stops_at_blob_boundary =
            generate_storage_block(0, &secret_key, 1, enough_bytes_to_almost_fill_a_blob());

        let requires_new_blob_but_doesnt_utilize_it =
            generate_storage_block(1, &secret_key, 1, enough_bytes_to_almost_fill_a_blob() / 3);

        let blocks: SequentialFuelBlocks = non_empty_vec![
            stops_at_blob_boundary,
            requires_new_blob_but_doesnt_utilize_it
        ]
        .try_into()
        .unwrap();

        let mut bundler = Bundler::new(
            Eip4844BlobEncoder,
            blocks.clone(),
            Compressor::no_compression(),
        );

        bundler.advance().await?;

        // when
        let non_optimal_bundle = proposal_if_finalized_now(&bundler).await;
        bundler.advance().await?;
        let optimal_bundle = bundler.finish().await?;

        // then
        assert_eq!(non_optimal_bundle.block_heights, 0..=1);
        assert!(!non_optimal_bundle.known_to_be_optimal);

        assert_eq!(optimal_bundle.block_heights, 0..=0);
        assert!(optimal_bundle.known_to_be_optimal);

        Ok(())
    }

    async fn proposal_if_finalized_now(bundler: &Bundler<Eip4844BlobEncoder>) -> BundleProposal {
        bundler.clone().finish().await.unwrap()
    }

    // This can happen when you've already paying for a blob but are not utilizing it. Adding
    // more data is going to increase the bytes per gas but keep the storage price the same.
    #[tokio::test]
    async fn wont_constrict_bundle_because_gas_remained_unchanged() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = generate_storage_block_sequence(0..=1, &secret_key, 10, 100);

        let mut bundler = Bundler::new(
            Eip4844BlobEncoder,
            blocks.clone(),
            Compressor::no_compression(),
        );

        while bundler.advance().await? {}

        // when
        let bundle = bundler.finish().await?;

        // then
        let bundle_data: NonEmptyVec<u8> = blocks
            .into_iter()
            .flat_map(|b| b.data.into_inner())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let expected_fragments = Eip4844BlobEncoder.encode(bundle_data).unwrap();

        assert!(bundle.known_to_be_optimal);
        assert_eq!(bundle.block_heights, 0..=1);
        assert_eq!(bundle.fragments, expected_fragments);

        Ok(())
    }

    fn enough_bytes_to_almost_fill_a_blob() -> usize {
        let encoding_overhead = Eip4844BlobEncoder::FRAGMENT_SIZE as f64 * 0.04;
        Eip4844BlobEncoder::FRAGMENT_SIZE - encoding_overhead as usize
    }

    // Because, for example, you've used up more of a whole blob you paid for
    #[tokio::test]
    async fn bigger_bundle_will_have_same_storage_gas_usage() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        let blocks = non_empty_vec![
            generate_storage_block(0, &secret_key, 0, 100),
            generate_storage_block(1, &secret_key, 1, enough_bytes_to_almost_fill_a_blob())
        ];

        let mut bundler = Bundler::new(
            Eip4844BlobEncoder,
            blocks.clone().try_into().unwrap(),
            Compressor::no_compression(),
        );

        while bundler.advance().await? {}

        // when
        let bundle = bundler.finish().await?;

        // then
        assert!(bundle.known_to_be_optimal);
        assert_eq!(bundle.block_heights, 0..=1);

        Ok(())
    }

    fn enough_bytes_to_almost_fill_entire_l1_tx() -> usize {
        let encoding_overhead = 20;
        let max_bytes_per_tx = Eip4844BlobEncoder::FRAGMENT_SIZE * 6;
        max_bytes_per_tx - encoding_overhead
    }
}
