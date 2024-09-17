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
    type Bundler: Bundle + Send + Sync;
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
        // TODO: segfault check against holes

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
        let storage_fee = u128::from(gas_usage.storage).saturating_mul(gas_prices.storage);
        let normal_fee = u128::from(gas_usage.normal).saturating_mul(gas_prices.normal);

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
    use std::sync::Arc;

    use crate::{
        state_committer::bundler::{Bundle, BundlerFactory, Compressor, Factory},
        test_utils, Result,
    };

    #[tokio::test]
    async fn not_calling_advance_gives_no_bundle() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let factory = Factory::new(
            Arc::new(ports::l1::MockApi::new()),
            setup.db(),
            1..2,
            Compressor::default(),
        )?;

        let bundler = factory.build().await?;

        // when
        let bundle = bundler.finish().await?;

        // then
        assert!(bundle.is_none());

        Ok(())
    }

    // TODO: segfault various tests around the logic
}
