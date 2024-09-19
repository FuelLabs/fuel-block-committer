use crate::Result;
use itertools::Itertools;

use flate2::{write::GzEncoder, Compression};
use ports::{
    l1::{GasPrices, GasUsage},
    storage::SequentialFuelBlocks,
    types::NonEmptyVec,
};
use std::{io::Write, num::NonZeroUsize, ops::RangeInclusive};

#[derive(Debug, Clone, Copy)]
pub struct Compressor {
    level: Compression,
}

#[allow(dead_code)]
pub enum Level {
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

impl Default for Compressor {
    fn default() -> Self {
        Self::new(Level::Level6)
    }
}

impl Compressor {
    pub fn new(level: Level) -> Self {
        let level = match level {
            Level::Min => 0,
            Level::Level1 => 1,
            Level::Level2 => 2,
            Level::Level3 => 3,
            Level::Level4 => 4,
            Level::Level5 => 5,
            Level::Level6 => 6,
            Level::Level7 => 7,
            Level::Level8 => 8,
            Level::Level9 => 9,
            Level::Max => 10,
        };

        Self {
            level: Compression::new(level),
        }
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
    async fn build(&self, blocks: SequentialFuelBlocks) -> Self::Bundler;
}

pub struct Factory<L1> {
    l1_adapter: L1,
    compressor: Compressor,
}

impl<L1> Factory<L1> {
    pub fn new(l1_adapter: L1, compressor: Compressor) -> Self {
        Self {
            l1_adapter,
            compressor,
        }
    }
}

#[async_trait::async_trait]
impl<L1> BundlerFactory for Factory<L1>
where
    L1: ports::l1::Api + Clone + Send + Sync + 'static,
{
    type Bundler = Bundler<L1>;

    async fn build(&self, blocks: SequentialFuelBlocks) -> Self::Bundler {
        Bundler::new(self.l1_adapter.clone(), blocks, self.compressor)
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
    blocks: NonEmptyVec<ports::storage::FuelBlock>,
    gas_usages: Vec<Proposal>, // Track all proposals
    current_block_count: NonZeroUsize,
    compressor: Compressor,
}

impl<L1> Bundler<L1>
where
    L1: ports::l1::Api + Send + Sync,
{
    pub fn new(l1_adapter: L1, blocks: SequentialFuelBlocks, compressor: Compressor) -> Self {
        Self {
            l1_adapter,
            blocks: blocks.into_inner(),
            gas_usages: Vec::new(),
            current_block_count: 1.try_into().expect("not zero"),
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
        let gas_usage = self
            .l1_adapter
            .gas_usage_to_store_data(compressed_data.len());

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
        let bundle_blocks = self.blocks_for_new_proposal();

        let proposal = self.create_proposal(bundle_blocks).await?;

        self.gas_usages.push(proposal);

        self.current_block_count = self.current_block_count.saturating_add(1);

        // Return whether there are more configurations to process
        Ok(self.current_block_count <= self.blocks.len())
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
        let max_data_per_fragment = self.l1_adapter.max_bytes_per_submission();

        // Calculate compression ratio
        let compression_ratio = self.calculate_compression_ratio(
            best_proposal.uncompressed_data_size,
            compressed_data.len(),
        );

        // Determine if all configurations have been tried
        let all_proposals_tried = self.current_block_count > self.blocks.len();

        let fragments = compressed_data
            .into_iter()
            .chunks(max_data_per_fragment.get())
            .into_iter()
            .map(|chunk| NonEmptyVec::try_from(chunk.collect_vec()).expect("should never be empty"))
            .collect_vec();

        let fragments = NonEmptyVec::try_from(fragments).expect("should never be empty");

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
    use std::{num::NonZeroUsize, sync::Arc};

    use itertools::Itertools;
    use ports::{
        l1::{Api as L1Api, GasPrices, GasUsage},
        non_empty_vec,
        storage::FuelBlock,
        types::{L1Height, NonEmptyVec, TransactionResponse, U256},
    };
    use rand::{rngs::SmallRng, Rng, SeedableRng};

    use crate::{
        state_committer::bundler::{Bundle, BundlerFactory, Compressor, Factory},
        Result,
    };

    // Mock L1 Adapter to control gas prices and usage during tests
    struct MockL1Adapter {
        gas_prices: GasPrices,
        gas_usage_per_byte: u64,
        max_bytes_per_submission: NonZeroUsize,
    }

    #[async_trait::async_trait]
    impl L1Api for MockL1Adapter {
        async fn gas_prices(&self) -> ports::l1::Result<GasPrices> {
            Ok(self.gas_prices)
        }

        fn gas_usage_to_store_data(&self, data_size: NonZeroUsize) -> GasUsage {
            GasUsage {
                storage: (data_size.get() as u64) * self.gas_usage_per_byte,
                normal: 0,
            }
        }

        fn max_bytes_per_submission(&self) -> NonZeroUsize {
            self.max_bytes_per_submission
        }

        async fn submit_l2_state(&self, _: NonEmptyVec<u8>) -> ports::l1::Result<[u8; 32]> {
            unimplemented!()
        }

        async fn get_block_number(&self) -> ports::l1::Result<L1Height> {
            unimplemented!()
        }

        async fn balance(&self) -> ports::l1::Result<U256> {
            unimplemented!()
        }
        async fn get_transaction_response(
            &self,
            _: [u8; 32],
        ) -> ports::l1::Result<Option<TransactionResponse>> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn not_calling_advance_gives_no_bundle() -> Result<()> {
        // Given
        let l1_adapter = MockL1Adapter {
            gas_prices: GasPrices {
                storage: 1,
                normal: 1,
            },
            gas_usage_per_byte: 1,
            max_bytes_per_submission: NonZeroUsize::new(1000).unwrap(),
        };
        let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());

        let sequence = non_empty_vec![FuelBlock {
            hash: [0; 32],
            height: 1,
            data: [0; 32].to_vec().try_into().unwrap(),
        }]
        .try_into()
        .unwrap();

        let bundler = factory.build(sequence).await;

        // When
        let bundle = bundler.finish().await?;

        // Then
        assert!(bundle.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn calling_advance_once_with_one_block_gives_a_bundle() -> Result<()> {
        // Given
        let l1_adapter = MockL1Adapter {
            gas_prices: GasPrices {
                storage: 1,
                normal: 1,
            },
            gas_usage_per_byte: 1,
            max_bytes_per_submission: NonZeroUsize::new(1000).unwrap(),
        };
        let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());

        let sequence = non_empty_vec![FuelBlock {
            hash: [0; 32],
            height: 1,
            data: [0; 32].to_vec().try_into().unwrap(),
        }]
        .try_into()
        .unwrap();

        let mut bundler = factory.build(sequence).await;

        // When
        let has_more = bundler.advance().await?;
        let bundle = bundler.finish().await?;

        // Then
        assert!(!has_more); // Since there is only one block
        assert!(bundle.is_some());

        // Also, check that the bundle contains the correct data
        let bundle = bundle.unwrap();
        assert_eq!(bundle.block_heights, 1..=1);
        assert!(bundle.optimal);

        Ok(())
    }

    #[tokio::test]
    async fn calling_advance_multiple_times_with_multiple_blocks_gives_optimal_bundle() -> Result<()>
    {
        // Given
        let l1_adapter = MockL1Adapter {
            gas_prices: GasPrices {
                storage: 1,
                normal: 1,
            },
            gas_usage_per_byte: 1,
            max_bytes_per_submission: NonZeroUsize::new(1000).unwrap(),
        };
        let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());

        let sequence = non_empty_vec![
            FuelBlock {
                hash: [0; 32],
                height: 1,
                data: [1; 32].to_vec().try_into().unwrap(),
            },
            FuelBlock {
                hash: [1; 32],
                height: 2,
                data: [2; 32].to_vec().try_into().unwrap(),
            },
            FuelBlock {
                hash: [2; 32],
                height: 3,
                data: [3; 32].to_vec().try_into().unwrap(),
            }
        ]
        .try_into()
        .unwrap();

        let mut bundler = factory.build(sequence).await;

        // When
        while bundler.advance().await? {}
        let bundle = bundler.finish().await?;

        // Then
        assert!(bundle.is_some());
        let bundle = bundle.unwrap();
        assert_eq!(bundle.block_heights, 1..=3);
        assert!(bundle.optimal);

        Ok(())
    }

    #[tokio::test]
    async fn calling_advance_few_times_with_multiple_blocks_gives_non_optimal_bundle() -> Result<()>
    {
        // Given
        let l1_adapter = MockL1Adapter {
            gas_prices: GasPrices {
                storage: 1,
                normal: 1,
            },
            gas_usage_per_byte: 1,
            max_bytes_per_submission: NonZeroUsize::new(1000).unwrap(),
        };
        let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());

        let sequence = non_empty_vec![
            FuelBlock {
                hash: [0; 32],
                height: 1,
                data: [1; 32].to_vec().try_into().unwrap(),
            },
            FuelBlock {
                hash: [1; 32],
                height: 2,
                data: [2; 32].to_vec().try_into().unwrap(),
            },
            FuelBlock {
                hash: [2; 32],
                height: 3,
                data: [3; 32].to_vec().try_into().unwrap(),
            }
        ]
        .try_into()
        .unwrap();

        let mut bundler = factory.build(sequence).await;

        // When
        let has_more = bundler.advance().await?; // Call advance only once
        let bundle = bundler.finish().await?;

        // Then
        assert!(has_more); // There should be more configurations to process
        assert!(bundle.is_some());
        let bundle = bundle.unwrap();
        assert_eq!(bundle.block_heights, 1..=1); // Should only include the first block
        assert!(!bundle.optimal); // Not all configurations were tried

        Ok(())
    }

    #[tokio::test]
    async fn bundler_selects_best_proposal_based_on_gas_prices() -> Result<()> {
        // Given different gas prices to affect the selection
        let gas_prices = GasPrices {
            storage: 10,
            normal: 1,
        };

        let l1_adapter = MockL1Adapter {
            gas_prices,
            gas_usage_per_byte: 1,
            max_bytes_per_submission: NonZeroUsize::new(1000).unwrap(),
        };

        let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());

        // Blocks with varying data sizes
        let sequence = non_empty_vec![
            FuelBlock {
                hash: [0; 32],
                height: 1,
                data: vec![0; 100].try_into().unwrap(),
            },
            FuelBlock {
                hash: [1; 32],
                height: 2,
                data: vec![1; 200].try_into().unwrap(),
            },
            FuelBlock {
                hash: [2; 32],
                height: 3,
                data: vec![2; 300].try_into().unwrap(),
            }
        ]
        .try_into()
        .unwrap();

        let mut bundler = factory.build(sequence).await;

        // When
        while bundler.advance().await? {}
        let bundle = bundler.finish().await?;

        // Then
        assert!(bundle.is_some());
        let bundle = bundle.unwrap();

        // With higher storage gas price, the bundler should select the proposal with the smallest data size per fee
        assert_eq!(bundle.block_heights, 1..=1);
        assert!(bundle.optimal);

        Ok(())
    }

    #[tokio::test]
    async fn compressor_compresses_data_correctly() -> Result<()> {
        // Given
        let compressor = Compressor::default();
        let data = vec![0u8; 1000];
        let data = NonEmptyVec::try_from(data).unwrap();

        // When
        let compressed_data = compressor.compress(data.clone()).await?;

        // Then
        assert!(compressed_data.len() < data.len());
        Ok(())
    }

    #[tokio::test]
    async fn bundler_handles_single_block_correctly() -> Result<()> {
        // Given
        let l1_adapter = MockL1Adapter {
            gas_prices: GasPrices {
                storage: 1,
                normal: 1,
            },
            gas_usage_per_byte: 1,
            max_bytes_per_submission: NonZeroUsize::new(1000).unwrap(),
        };
        let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());

        let sequence = non_empty_vec![FuelBlock {
            hash: [0; 32],
            height: 42,
            data: vec![0; 100].try_into().unwrap(),
        }]
        .try_into()
        .unwrap();

        let mut bundler = factory.build(sequence).await;

        // When
        bundler.advance().await?;
        let bundle = bundler.finish().await?;

        // Then
        assert!(bundle.is_some());
        let bundle = bundle.unwrap();
        assert_eq!(bundle.block_heights, 42..=42);
        assert!(bundle.optimal);

        Ok(())
    }

    #[tokio::test]
    async fn bundler_splits_data_into_fragments_correctly() -> Result<()> {
        // Given
        let l1_adapter = MockL1Adapter {
            gas_prices: GasPrices {
                storage: 1,
                normal: 1,
            },
            gas_usage_per_byte: 1,
            max_bytes_per_submission: NonZeroUsize::new(50).unwrap(),
        };
        let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());

        let mut data = vec![0; 200];
        let mut rng = SmallRng::from_seed([0; 32]);
        rng.fill(&mut data[..]);

        let sequence = non_empty_vec![FuelBlock {
            hash: [0; 32],
            height: 1,
            data: data.try_into().unwrap(),
        }]
        .try_into()
        .unwrap();

        let mut bundler = factory.build(sequence).await;

        // When
        bundler.advance().await?;
        let bundle = bundler.finish().await?;

        // Then
        assert!(bundle.is_some());
        let bundle = bundle.unwrap();
        assert!(bundle.fragments.len().get() > 1);
        assert!(bundle
            .fragments
            .iter()
            .all(|fragment| fragment.len().get() <= 50));

        Ok(())
    }
}
