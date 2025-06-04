use std::{fmt::Display, num::NonZeroUsize, ops::RangeInclusive};

use crate::{
    Result,
    types::{Fragment, NonEmpty, NonNegative, storage::SequentialFuelBlocks},
};
use bytesize::ByteSize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub block_heights: RangeInclusive<u32>,
    pub known_to_be_optimal: bool,
    pub block_num_upper_limit: NonZeroUsize,
    pub optimization_attempts: usize,
    pub gas_usage: u64,
    pub compressed_data_size: NonZeroUsize,
    pub uncompressed_data_size: NonZeroUsize,
    pub num_fragments: NonZeroUsize,
}

impl Metadata {
    pub fn num_blocks(&self) -> usize {
        self.block_heights.clone().count()
    }

    // This is for metrics anyway, precision loss is ok
    #[allow(clippy::cast_precision_loss)]
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
            .field("block_num_upper_limit", &self.block_num_upper_limit)
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    async fn build(&self, blocks: SequentialFuelBlocks, id: NonNegative<i32>) -> Self::Bundler;
}
