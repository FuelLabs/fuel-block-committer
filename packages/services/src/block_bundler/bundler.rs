use std::{
    cmp::min, collections::VecDeque, fmt::Display, io::Write, num::NonZeroUsize,
    ops::RangeInclusive, str::FromStr,
};

use bytesize::ByteSize;
use flate2::{write::GzEncoder, Compression};
use itertools::Itertools;
use ports::{
    l1::FragmentEncoder,
    storage::SequentialFuelBlocks,
    types::{CollectNonEmpty, Fragment, NonEmpty},
};
use rayon::prelude::*;

use crate::Result;

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

    pub fn compress(&self, data: NonEmpty<u8>) -> Result<NonEmpty<u8>> {
        let Some(level) = self.compression else {
            return Ok(data);
        };

        let bytes = Vec::from(data);

        let mut encoder = GzEncoder::new(Vec::new(), level);
        encoder
            .write_all(&bytes)
            .map_err(|e| crate::Error::Other(e.to_string()))?;

        encoder
            .finish()
            .map_err(|e| crate::Error::Other(e.to_string()))?
            .into_iter()
            .collect_nonempty()
            .ok_or_else(|| crate::Error::Other("compression resulted in no data".to_string()))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Metadata {
    pub block_heights: RangeInclusive<u32>,
    pub known_to_be_optimal: bool,
    pub optimization_attempts: u64,
    pub gas_usage: u64,
    pub compressed_data_size: NonZeroUsize,
    pub uncompressed_data_size: NonZeroUsize,
    pub num_fragments: NonZeroUsize,
}

impl Metadata {
    pub fn num_blocks(&self) -> usize {
        self.block_heights.clone().count()
    }

    pub fn compression_ratio(&self) -> f64 {
        self.uncompressed_data_size.get() as f64 / self.compressed_data_size.get() as f64
    }
}

impl Display for Metadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metadata")
            .field("num_blocks", &self.num_blocks())
            .field("block_heights", &self.block_heights)
            .field("known_to_be_optimal", &self.known_to_be_optimal)
            .field("optimization_attempts", &self.optimization_attempts)
            .field("gas_usage", &self.gas_usage)
            .field(
                "compressed_data_size",
                &ByteSize(self.compressed_data_size.get() as u64),
            )
            .field(
                "uncompressed_data_size",
                &ByteSize(self.uncompressed_data_size.get() as u64),
            )
            .field("compression_ratio", &self.compression_ratio())
            .field("num_fragments", &self.num_fragments.get())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BundleProposal {
    pub fragments: NonEmpty<Fragment>,
    pub metadata: Metadata,
}

#[trait_variant::make(Send)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Bundle {
    /// Attempts to advance the bundler by trying out a new bundle configuration.
    ///
    /// Returns `true` if there are more configurations to process, or `false` otherwise.
    async fn advance(&mut self, num_concurrent: NonZeroUsize) -> Result<bool>;

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
    step_size: NonZeroUsize,
}

impl<L1> Factory<L1> {
    pub fn new(gas_calc: L1, compression_level: CompressionLevel, step_size: NonZeroUsize) -> Self {
        Self {
            gas_calc,
            compression_level,
            step_size,
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
            self.step_size,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Proposal {
    block_heights: RangeInclusive<u32>,
    uncompressed_data_size: NonZeroUsize,
    compressed_data: NonEmpty<u8>,
    gas_usage: u64,
}

impl Proposal {
    fn gas_per_uncompressed_byte(&self) -> f64 {
        self.gas_usage as f64 / self.uncompressed_data_size.get() as f64
    }
}

#[derive(Debug, Clone)]
pub struct Bundler<FragmentEncoder> {
    fragment_encoder: FragmentEncoder,
    blocks: NonEmpty<ports::storage::DBCompressedBlock>,
    best_proposal: Option<Proposal>,
    compressor: Compressor,
    attempts: VecDeque<NonZeroUsize>,
}

impl<T> Bundler<T> {
    fn new(
        cost_calculator: T,
        blocks: SequentialFuelBlocks,
        compressor: Compressor,
        initial_step_size: NonZeroUsize,
    ) -> Self {
        let max_blocks = blocks.len();
        let initial_step = initial_step_size;

        let attempts = generate_attempts(max_blocks, initial_step);

        Self {
            fragment_encoder: cost_calculator,
            blocks: blocks.into_inner(),
            best_proposal: None,
            compressor,
            attempts,
        }
    }

    fn save_if_best_so_far(&mut self, new_proposal: Proposal) {
        match &mut self.best_proposal {
            Some(best)
                if new_proposal.gas_per_uncompressed_byte() < best.gas_per_uncompressed_byte() =>
            {
                *best = new_proposal;
            }
            None => {
                self.best_proposal = Some(new_proposal);
            }
            _ => {}
        }
    }

    fn blocks_for_new_proposal(
        &self,
        block_count: NonZeroUsize,
    ) -> NonEmpty<ports::storage::DBCompressedBlock> {
        self.blocks
            .iter()
            .take(block_count.get())
            .cloned()
            .collect_nonempty()
            .expect("non-empty")
    }

    fn blocks_bundles_for_analyzing(
        &mut self,
        num_concurrent: std::num::NonZero<usize>,
    ) -> Vec<NonEmpty<ports::storage::DBCompressedBlock>> {
        let mut blocks_for_attempts = vec![];

        while !self.attempts.is_empty() && blocks_for_attempts.len() < num_concurrent.get() {
            let block_count = self.attempts.pop_front().expect("not empty");
            let blocks = self.blocks_for_new_proposal(block_count);
            blocks_for_attempts.push(blocks);
        }
        blocks_for_attempts
    }

    async fn analyze(&mut self, num_concurrent: std::num::NonZero<usize>) -> Result<Vec<Proposal>>
    where
        T: ports::l1::FragmentEncoder + Send + Sync + Clone + 'static,
    {
        let blocks_for_analyzing = self.blocks_bundles_for_analyzing(num_concurrent);

        let compressor = self.compressor;
        let fragment_encoder = self.fragment_encoder.clone();

        // Needs to be wrapped in a blocking task to avoid blocking the executor
        tokio::task::spawn_blocking(move || {
            blocks_for_analyzing
                .into_par_iter()
                .map(|blocks| {
                    let fragment_encoder = fragment_encoder.clone();
                    create_proposal(compressor, fragment_encoder, blocks)
                })
                .collect::<Result<Vec<_>>>()
        })
        .await
        .map_err(|e| crate::Error::Other(e.to_string()))?
    }
}

impl<T> Bundle for Bundler<T>
where
    T: ports::l1::FragmentEncoder + Send + Sync + Clone + 'static,
{
    async fn advance(&mut self, optimization_runs: NonZeroUsize) -> Result<bool> {
        if self.attempts.is_empty() {
            return Ok(false);
        }

        for proposal in self.analyze(optimization_runs).await? {
            self.save_if_best_so_far(proposal);
        }

        Ok(!self.attempts.is_empty())
    }

    async fn finish(mut self) -> Result<BundleProposal> {
        if self.best_proposal.is_none() {
            self.advance(1.try_into().expect("not zero")).await?;
        }

        let best_proposal = self
            .best_proposal
            .take()
            .expect("advance should have set the best proposal");

        let compressed_data_size = best_proposal.compressed_data.len_nonzero();
        let fragments = self
            .fragment_encoder
            .encode(best_proposal.compressed_data)?;

        let num_attempts = self
            .blocks
            .len()
            .saturating_sub(self.attempts.len())
            .try_into()
            .map_err(|_| crate::Error::Other("too many attempts".to_string()))?;

        Ok(BundleProposal {
            metadata: Metadata {
                block_heights: best_proposal.block_heights,
                known_to_be_optimal: self.attempts.is_empty(),
                uncompressed_data_size: best_proposal.uncompressed_data_size,
                compressed_data_size,
                gas_usage: best_proposal.gas_usage,
                optimization_attempts: num_attempts,
                num_fragments: fragments.len_nonzero(),
            },
            fragments,
        })
    }
}

// The step sizes are progressively halved, starting from the largest step, in order to explore
// bundling opportunities more efficiently. Larger steps attempt to bundle more blocks together,
// which can result in significant gas savings or better compression ratios early on.
// By starting with the largest step, we cover more ground and are more likely to encounter
// major improvements quickly. As the step size decreases, the search becomes more fine-tuned,
// focusing on incremental changes. This ensures that even if we stop optimizing early, we will
// have tested configurations that likely provide substantial benefits to the overall gas per byte.
fn generate_attempts(
    max_blocks: NonZeroUsize,
    initial_step: NonZeroUsize,
) -> VecDeque<NonZeroUsize> {
    std::iter::successors(Some(min(initial_step, max_blocks).get()), |&step| {
        (step > 1).then_some(step / 2)
    })
    .flat_map(|step| (1..=max_blocks.get()).rev().step_by(step))
    .filter_map(NonZeroUsize::new)
    .unique()
    .collect()
}

fn merge_block_data(blocks: NonEmpty<ports::storage::DBCompressedBlock>) -> NonEmpty<u8> {
    blocks
        .into_iter()
        .flat_map(|b| b.data)
        .collect_nonempty()
        .expect("non-empty")
}

fn create_proposal(
    compressor: Compressor,
    fragment_encoder: impl FragmentEncoder,
    bundle_blocks: NonEmpty<ports::storage::DBCompressedBlock>,
) -> Result<Proposal> {
    let block_heights = bundle_blocks.first().height..=bundle_blocks.last().height;

    let uncompressed_data = merge_block_data(bundle_blocks);
    let uncompressed_data_size = uncompressed_data.len_nonzero();

    let compressed_data = compressor.compress(uncompressed_data)?;

    let gas_usage = fragment_encoder.gas_usage(compressed_data.len_nonzero());

    Ok(Proposal {
        uncompressed_data_size,
        compressed_data,
        gas_usage,
        block_heights,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use eth::Eip4844BlobEncoder;
    use fuel_crypto::SecretKey;
    use ports::{l1::FragmentEncoder, types::nonempty};

    use super::*;
    use crate::test_utils::mocks::fuel::{generate_storage_block, generate_storage_block_sequence};

    #[test]
    fn can_disable_compression() {
        // given
        let compressor = Compressor::new(CompressionLevel::Disabled);
        let data = nonempty!(1, 2, 3);

        // when
        let compressed = compressor.compress(data.clone()).unwrap();

        // then
        assert_eq!(data, compressed);
    }

    #[test]
    fn all_compression_levels_work() {
        let data = nonempty!(1, 2, 3);
        for level in CompressionLevel::levels() {
            let compressor = Compressor::new(level);
            compressor.compress(data.clone()).unwrap();
        }
    }

    #[tokio::test]
    async fn finishing_will_advance_if_not_called_at_least_once() {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = generate_storage_block_sequence(0..=10, &secret_key, 10, 100);

        let bundler = Bundler::new(
            Eip4844BlobEncoder,
            blocks.clone(),
            Compressor::no_compression(),
            NonZeroUsize::new(1).unwrap(),
        );

        // when
        let bundle = bundler.finish().await.unwrap();

        // then
        let merged = blocks.into_inner().flat_map(|b| b.data.clone());
        let expected_fragments = Eip4844BlobEncoder.encode(merged).unwrap();
        assert!(!bundle.metadata.known_to_be_optimal);
        assert_eq!(bundle.metadata.block_heights, 0..=10);
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

        let blocks: SequentialFuelBlocks = nonempty![
            stops_at_blob_boundary,
            requires_new_blob_but_doesnt_utilize_it
        ]
        .try_into()
        .unwrap();

        let mut bundler = Bundler::new(
            Eip4844BlobEncoder,
            blocks.clone(),
            Compressor::no_compression(),
            NonZeroUsize::new(1).unwrap(),
        );

        bundler.advance(1.try_into().unwrap()).await?;

        let non_optimal_bundle = bundler.clone().finish().await?;
        bundler.advance(1.try_into().unwrap()).await?;

        // when
        let optimal_bundle = bundler.finish().await?;

        // then
        // Non-optimal bundle should include both blocks
        assert_eq!(non_optimal_bundle.metadata.block_heights, 0..=1);
        assert!(!non_optimal_bundle.metadata.known_to_be_optimal);

        // Optimal bundle should include only the first block
        assert_eq!(optimal_bundle.metadata.block_heights, 0..=0);
        assert!(optimal_bundle.metadata.known_to_be_optimal);

        Ok(())
    }

    #[tokio::test]
    async fn tolerates_step_too_large() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        let blocks = generate_storage_block_sequence(0..=2, &secret_key, 3, 100);

        let step_size = NonZeroUsize::new(5).unwrap(); // Step size larger than number of blocks

        let mut bundler = Bundler::new(
            Eip4844BlobEncoder,
            blocks.clone(),
            Compressor::no_compression(),
            step_size,
        );

        while bundler.advance(1.try_into().unwrap()).await? {}

        // when
        let bundle = bundler.finish().await?;

        // then
        assert!(bundle.metadata.known_to_be_optimal);
        assert_eq!(bundle.metadata.block_heights, 0..=2);
        assert_eq!(bundle.metadata.optimization_attempts, 3); // 3 then 1 then 2
        Ok(())
    }

    // when the smaller bundle doesn't utilize the whole blob, for example
    #[tokio::test]
    async fn bigger_bundle_will_have_same_storage_gas_usage() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        let blocks = nonempty![
            generate_storage_block(0, &secret_key, 1, 100),
            generate_storage_block(1, &secret_key, 1, enough_bytes_to_almost_fill_a_blob())
        ];

        let mut bundler = Bundler::new(
            Eip4844BlobEncoder,
            blocks.clone().try_into().unwrap(),
            Compressor::no_compression(),
            NonZeroUsize::new(1).unwrap(), // Default step size
        );
        while bundler.advance(1.try_into().unwrap()).await? {}

        // when
        let bundle = bundler.finish().await?;

        // then
        assert!(bundle.metadata.known_to_be_optimal);
        assert_eq!(bundle.metadata.block_heights, 0..=1);
        Ok(())
    }

    fn enough_bytes_to_almost_fill_a_blob() -> usize {
        let encoding_overhead = Eip4844BlobEncoder::FRAGMENT_SIZE as f64 * 0.04;
        Eip4844BlobEncoder::FRAGMENT_SIZE - encoding_overhead as usize
    }
    #[test]
    fn generates_steps_as_expected() {
        // given
        let max_steps = 100;
        let max_step = 20;

        // when
        let steps = generate_attempts(
            NonZeroUsize::new(max_steps).unwrap(),
            NonZeroUsize::new(max_step).unwrap(),
        );

        // then
        let actual_steps = steps.into_iter().map(|s| s.get()).collect::<Vec<_>>();
        let expected_steps = vec![
            100, 80, 60, 40, 20, 90, 70, 50, 30, 10, 95, 85, 75, 65, 55, 45, 35, 25, 15, 5, 98, 96,
            94, 92, 88, 86, 84, 82, 78, 76, 74, 72, 68, 66, 64, 62, 58, 56, 54, 52, 48, 46, 44, 42,
            38, 36, 34, 32, 28, 26, 24, 22, 18, 16, 14, 12, 8, 6, 4, 2, 99, 97, 93, 91, 89, 87, 83,
            81, 79, 77, 73, 71, 69, 67, 63, 61, 59, 57, 53, 51, 49, 47, 43, 41, 39, 37, 33, 31, 29,
            27, 23, 21, 19, 17, 13, 11, 9, 7, 3, 1,
        ];

        assert_eq!(actual_steps, expected_steps);
    }
}
