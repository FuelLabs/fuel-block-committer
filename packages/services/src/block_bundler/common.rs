use std::{fmt::Display, num::NonZeroUsize, ops::RangeInclusive};

use crate::{
    types::{storage::SequentialFuelBlocks, Fragment, NonEmpty, NonNegative},
    Result,
};

#[derive(Debug, Clone, PartialEq, Eq)]
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

    // This is for metrics anyway, precision loss is ok
    #[allow(clippy::cast_precision_loss)]
    pub fn compression_ratio(&self) -> f64 {
        self.uncompressed_data_size.get() as f64 / self.compressed_data_size.get() as f64
    }
}

impl Display for Metadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metadata")
            .field("num_blocks", &self.block_heights.clone().count())
            .field("block_heights", &self.block_heights)
            .field("known_to_be_optimal", &self.known_to_be_optimal)
            .field("optimization_attempts", &self.optimization_attempts)
            .field("gas_usage", &self.gas_usage)
            .field("compressed_data_size", &self.compressed_data_size)
            .field("uncompressed_data_size", &self.uncompressed_data_size)
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
pub trait Bundle {
    /// Attempts to advance the bundler by trying out a new bundle configuration.
    ///
    /// Returns `true` if there are more configurations to process, or `false` otherwise.
    async fn advance(&mut self, num_concurrent: NonZeroUsize) -> Result<bool>;

    /// Finalizes the bundling process by selecting the best bundle based on current settings.
    ///
    /// Consumes the bundler.
    async fn finish(self) -> Result<BundleProposal>;
}

#[trait_variant::make(Send)]
pub trait BundlerFactory {
    type Bundler: Bundle + Send + Sync;
    async fn build(&self, blocks: SequentialFuelBlocks, id: NonNegative<i32>) -> Self::Bundler;
}
