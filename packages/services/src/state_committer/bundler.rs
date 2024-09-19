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
    compression: Option<Compression>,
}

#[allow(dead_code)]
pub enum Level {
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

impl Level {
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
        Self::new(Level::Level6)
    }
}

impl Compressor {
    pub fn no_compression() -> Self {
        Self::new(Level::Disabled)
    }

    pub fn new(level: Level) -> Self {
        let level = match level {
            Level::Disabled => None,
            Level::Min => Some(0),
            Level::Level1 => Some(1),
            Level::Level2 => Some(2),
            Level::Level3 => Some(3),
            Level::Level4 => Some(4),
            Level::Level5 => Some(5),
            Level::Level6 => Some(6),
            Level::Level7 => Some(7),
            Level::Level8 => Some(8),
            Level::Level9 => Some(9),
            Level::Max => Some(10),
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
    pub optimal: bool,
    pub compression_ratio: f64,
    pub gas_usage: GasUsage,
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
    async fn finish(self, gas_prices: GasPrices) -> Result<BundleProposal>;
}

#[async_trait::async_trait]
pub trait BundlerFactory {
    type Bundler: Bundle + Send + Sync;
    async fn build(&self, blocks: SequentialFuelBlocks) -> Self::Bundler;
}

pub struct Factory<GasCalculator> {
    gas_calc: GasCalculator,
    compressor: Compressor,
}

impl<L1> Factory<L1> {
    pub fn new(gas_calc: L1, compressor: Compressor) -> Self {
        Self {
            gas_calc,
            compressor,
        }
    }
}

#[async_trait::async_trait]
impl<GasCalculator> BundlerFactory for Factory<GasCalculator>
where
    GasCalculator: ports::l1::StorageCostCalculator + Clone + Send + Sync + 'static,
{
    type Bundler = Bundler<GasCalculator>;

    async fn build(&self, blocks: SequentialFuelBlocks) -> Self::Bundler {
        Bundler::new(self.gas_calc.clone(), blocks, self.compressor)
    }
}

/// Represents a bundle configuration and its associated gas usage.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Proposal {
    num_blocks: NonZeroUsize,
    uncompressed_data_size: NonZeroUsize,
    compressed_data_size: NonZeroUsize,
    gas_usage: GasUsage,
}

#[derive(Debug, Clone)]
pub struct Bundler<CostCalc> {
    cost_calculator: CostCalc,
    blocks: NonEmptyVec<ports::storage::FuelBlock>,
    gas_usages: Vec<Proposal>, // Track all proposals
    current_block_count: NonZeroUsize,
    compressor: Compressor,
}

impl<T> Bundler<T>
where
    T: ports::l1::StorageCostCalculator + Send + Sync,
{
    pub fn new(cost_calculator: T, blocks: SequentialFuelBlocks, compressor: Compressor) -> Self {
        Self {
            cost_calculator,
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
            .cost_calculator
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
impl<T> Bundle for Bundler<T>
where
    T: ports::l1::StorageCostCalculator + Send + Sync,
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
    async fn finish(mut self, gas_prices: GasPrices) -> Result<BundleProposal> {
        if self.gas_usages.is_empty() {
            self.advance().await?;
        }

        // Select the best proposal based on current gas prices
        let best_proposal = self.select_best_proposal(&gas_prices)?;

        // Determine the block height range based on the number of blocks in the best proposal
        let block_heights = self.calculate_block_heights(best_proposal.num_blocks)?;

        // Recompress the best bundle's data
        let compressed_data = self
            .compress_first_n_blocks(best_proposal.num_blocks)
            .await?;

        // Split into submittable fragments
        let max_data_per_fragment = self.cost_calculator.max_bytes_per_submission();

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

        Ok(BundleProposal {
            fragments,
            block_heights,
            optimal: all_proposals_tried,
            compression_ratio,
            gas_usage: best_proposal.gas_usage,
        })
    }
}

#[cfg(test)]
mod tests {

    use eth::Eip4844GasUsage;
    use flate2::Compress;
    use fuel_crypto::{Message, SecretKey, Signature};
    use ports::l1::StorageCostCalculator;
    use ports::non_empty_vec;

    use crate::test_utils::{
        self,
        mocks::fuel::{generate_storage_block, generate_storage_block_sequence},
    };

    use super::*;

    #[test]
    fn can_disable_compression() {
        // given
        let compressor = Compressor::new(Level::Disabled);
        let data = non_empty_vec!(1, 2, 3);

        // when
        let compressed = compressor.compress_blocking(&data).unwrap();

        // then
        assert_eq!(data, compressed);
    }

    #[test]
    fn all_compression_levels_work() {
        let data = non_empty_vec!(1, 2, 3);
        for level in Level::levels() {
            let compressor = Compressor::new(level);
            compressor.compress_blocking(&data).unwrap();
        }
    }

    #[tokio::test]
    async fn finishing_will_advance_if_not_called_at_least_once() {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = generate_storage_block_sequence(0..=0, &secret_key, 10);

        let bundler = Bundler::new(
            Eip4844GasUsage,
            blocks.clone(),
            Compressor::no_compression(),
        );

        // when
        let bundle = bundler
            .finish(GasPrices {
                storage: 10,
                normal: 1,
            })
            .await
            .unwrap();

        // then
        let expected_fragment = blocks[0].data.clone();
        assert!(bundle.optimal);
        assert_eq!(bundle.block_heights, 0..=0);
        assert_eq!(bundle.fragments, non_empty_vec![expected_fragment]);
    }

    #[tokio::test]
    async fn will_provide_a_suboptimal_bundle_if_not_advanced_enough() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = generate_storage_block_sequence(0..=3, &secret_key, 10);

        let mut bundler = Bundler::new(
            Eip4844GasUsage,
            blocks.clone(),
            Compressor::no_compression(),
        );
        bundler.advance().await?;

        // when
        let bundle = bundler
            .finish(GasPrices {
                storage: 10,
                normal: 1,
            })
            .await?;

        // then
        let expected_fragment = blocks[0].data.clone();
        assert!(!bundle.optimal);
        assert_eq!(bundle.block_heights, 0..=0);
        assert_eq!(bundle.fragments, non_empty_vec![expected_fragment]);

        Ok(())
    }

    async fn proposal_if_finalized_now(
        bundler: &Bundler<Eip4844GasUsage>,
        price: GasPrices,
    ) -> BundleProposal {
        bundler.clone().finish(price).await.unwrap()
    }

    // This can happen when you've already paying for a blob but are not utilizing it. Adding
    // more data is going to increase the bytes per gas but keep the storage price the same.
    #[tokio::test]
    async fn will_expand_bundle_because_storage_gas_remained_unchanged() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = generate_storage_block_sequence(0..=1, &secret_key, 10);

        let mut bundler = Bundler::new(
            Eip4844GasUsage,
            blocks.clone(),
            Compressor::no_compression(),
        );

        let price = GasPrices {
            storage: 10,
            normal: 1,
        };
        bundler.advance().await?;
        let single_block_proposal = proposal_if_finalized_now(&bundler, price).await;

        bundler.advance().await?;
        // when
        let bundle = bundler.finish(price).await?;

        // then
        let expected_fragment: NonEmptyVec<u8> = blocks
            .into_iter()
            .flat_map(|b| b.data)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        assert!(expected_fragment.len() < Eip4844GasUsage.max_bytes_per_submission());

        assert!(bundle.optimal);
        assert_eq!(bundle.block_heights, 0..=1);
        assert_eq!(bundle.fragments, non_empty_vec![expected_fragment]);

        assert_eq!(single_block_proposal.block_heights, 0..=0);
        assert_eq!(single_block_proposal.gas_usage, bundle.gas_usage);

        Ok(())
    }

    fn enough_txs_to_almost_fill_a_blob() -> usize {
        let encoding_overhead = 40;
        let blobs_per_block = 6;
        let tx_size = 32;
        let max_bytes_per_tx = Eip4844GasUsage.max_bytes_per_submission().get();
        (max_bytes_per_tx / blobs_per_block - encoding_overhead) / tx_size
    }

    // When, for example, you need to pay for a new blob but dont have that much extra data
    #[tokio::test]
    async fn adding_a_block_will_worsen_storage_gas_usage() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        let enough_tx_to_spill_into_second_blob = 1;
        let blocks = non_empty_vec![
            generate_storage_block(0, &secret_key, enough_txs_to_almost_fill_a_blob()),
            generate_storage_block(1, &secret_key, enough_tx_to_spill_into_second_blob)
        ];

        let mut bundler = Bundler::new(
            Eip4844GasUsage,
            blocks.clone().try_into().unwrap(),
            Compressor::no_compression(),
        );

        bundler.advance().await?;
        bundler.advance().await?;

        // when
        let bundle = bundler
            .finish(GasPrices {
                storage: 10,
                normal: 1,
            })
            .await?;

        // then
        let expected_fragment = &blocks.first().data;

        assert!(bundle.optimal);
        assert_eq!(bundle.block_heights, 0..=0);
        assert_eq!(bundle.fragments, non_empty_vec![expected_fragment.clone()]);

        Ok(())
    }

    fn enough_txs_to_almost_fill_entire_l1_tx() -> usize {
        let encoding_overhead = 20;
        let tx_size = 32;
        let max_bytes_per_tx = Eip4844GasUsage.max_bytes_per_submission().get();
        (max_bytes_per_tx - encoding_overhead) / tx_size
    }

    // When, for example, you might have enough data to fill a new blob but the cost of the extra
    // l1 tx outweights the benefit
    #[tokio::test]
    async fn adding_a_block_results_in_worse_gas_due_to_extra_tx() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        let enough_tx_to_spill_into_second_tx = 1;
        let blocks = non_empty_vec![
            generate_storage_block(0, &secret_key, enough_txs_to_almost_fill_entire_l1_tx()),
            generate_storage_block(1, &secret_key, enough_tx_to_spill_into_second_tx)
        ];

        let mut bundler = Bundler::new(
            Eip4844GasUsage,
            blocks.clone().try_into().unwrap(),
            Compressor::no_compression(),
        );

        while bundler.advance().await? {}

        // when
        let bundle = bundler
            .finish(GasPrices {
                storage: 10,
                normal: 1,
            })
            .await?;

        // then
        let expected_fragment = &blocks.first().data;

        assert!(bundle.optimal);
        assert_eq!(bundle.block_heights, 0..=0);
        assert_eq!(bundle.fragments, non_empty_vec![expected_fragment.clone()]);

        Ok(())
    }

    // When, for example, adding new blocks to the bundle will cause a second l1 tx but the overall
    // compression will make up for the extra cost
    #[tokio::test]
    async fn adding_a_block_results_in_a_new_tx_but_better_compression() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        let enough_tx_to_make_up_for_the_extra_cost = 100000;
        // we lose some space since the first block is not compressible
        let compression_overhead = 4;
        let non_compressable_block = generate_storage_block(
            0,
            &secret_key,
            enough_txs_to_almost_fill_entire_l1_tx() - compression_overhead,
        );

        let compressable_block = {
            let mut block =
                generate_storage_block(1, &secret_key, enough_tx_to_make_up_for_the_extra_cost);
            block.data.fill(0);
            block
        };

        let blocks = non_empty_vec![non_compressable_block, compressable_block];

        let mut bundler = Bundler::new(
            Eip4844GasUsage,
            blocks.clone().try_into().unwrap(),
            Compressor::default(),
        );

        bundler.advance().await?;
        let price = GasPrices {
            storage: 10,
            normal: 1,
        };
        let single_block_proposal = proposal_if_finalized_now(&bundler, price).await;
        bundler.advance().await?;

        // when
        let bundle = bundler
            .finish(GasPrices {
                storage: 10,
                normal: 1,
            })
            .await?;

        // then
        assert!(bundle.optimal);
        assert_eq!(bundle.block_heights, 0..=1);
        assert_eq!(
            bundle.gas_usage.normal,
            2 * single_block_proposal.gas_usage.normal
        );

        Ok(())
    }
}

// #[cfg(test)]
// mod tests {
//     use std::{num::NonZeroUsize, sync::Arc};
//
//     use itertools::Itertools;
//     use ports::{
//         l1::{Api as L1Api, GasPrices, GasUsage},
//         non_empty_vec,
//         storage::FuelBlock,
//         types::{L1Height, NonEmptyVec, TransactionResponse, U256},
//     };
//
//     use crate::{
//         state_committer::bundler::{Bundle, BundlerFactory, Compressor, Factory},
//         Result,
//     };
//
//     // Mock L1 Adapter to control gas prices and usage during tests
//     struct MockL1Adapter {
//         gas_prices: GasPrices,
//         gas_usage_per_byte: u64,
//         max_bytes_per_submission: NonZeroUsize,
//         // Overhead after reaching a certain data size
//         overhead_threshold: usize,
//         overhead_gas: u64,
//     }
//
//     #[tokio::test]
//     async fn bundler_with_easily_compressible_data_prefers_larger_bundle() -> Result<()> {
//         // Given
//         let l1_adapter = MockL1Adapter {
//             gas_prices: GasPrices {
//                 storage: 1,
//                 normal: 1,
//             },
//             gas_usage_per_byte: 1,
//             max_bytes_per_submission: NonZeroUsize::new(1000).unwrap(),
//             overhead_threshold: 0, // No overhead in this test
//             overhead_gas: 0,
//         };
//         let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());
//
//         // Easily compressible data (repeating patterns)
//         let block_data = vec![0u8; 1000]; // Large block with zeros, highly compressible
//         let sequence = non_empty_vec![
//             FuelBlock {
//                 hash: [0; 32],
//                 height: 1,
//                 data: block_data.clone().try_into().unwrap(),
//             },
//             FuelBlock {
//                 hash: [1; 32],
//                 height: 2,
//                 data: block_data.clone().try_into().unwrap(),
//             },
//             FuelBlock {
//                 hash: [2; 32],
//                 height: 3,
//                 data: block_data.clone().try_into().unwrap(),
//             }
//         ]
//         .try_into()
//         .unwrap();
//
//         let mut bundler = factory.build(sequence).await;
//
//         // When
//         while bundler.advance().await? {}
//         let bundle = bundler.finish().await?;
//
//         // Then
//         assert!(bundle.is_some());
//         let bundle = bundle.unwrap();
//
//         // The bundler should include all blocks because adding more compressible data improves gas per byte
//         assert_eq!(bundle.block_heights, 1..=3);
//         assert!(bundle.optimal);
//
//         Ok(())
//     }
//
//     #[tokio::test]
//     async fn bundler_with_random_data_prefers_smaller_bundle() -> Result<()> {
//         // Given
//         let l1_adapter = MockL1Adapter {
//             gas_prices: GasPrices {
//                 storage: 1,
//                 normal: 1,
//             },
//             gas_usage_per_byte: 1,
//             max_bytes_per_submission: NonZeroUsize::new(1000).unwrap(),
//             overhead_threshold: 0,
//             overhead_gas: 0,
//         };
//         let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());
//
//         // Random data (not compressible)
//         use rand::{RngCore, SeedableRng};
//         let mut rng = rand::rngs::StdRng::seed_from_u64(42);
//
//         let block1_data: Vec<u8> = (0..1000).map(|_| rng.next_u32() as u8).collect();
//         let block2_data: Vec<u8> = (0..1000).map(|_| rng.next_u32() as u8).collect();
//         let block3_data: Vec<u8> = (0..1000).map(|_| rng.next_u32() as u8).collect();
//
//         let sequence = non_empty_vec![
//             FuelBlock {
//                 hash: [0; 32],
//                 height: 1,
//                 data: block1_data.try_into().unwrap(),
//             },
//             FuelBlock {
//                 hash: [1; 32],
//                 height: 2,
//                 data: block2_data.try_into().unwrap(),
//             },
//             FuelBlock {
//                 hash: [2; 32],
//                 height: 3,
//                 data: block3_data.try_into().unwrap(),
//             }
//         ]
//         .try_into()
//         .unwrap();
//
//         let mut bundler = factory.build(sequence).await;
//
//         // When
//         while bundler.advance().await? {}
//         let bundle = bundler.finish().await?;
//
//         // Then
//         assert!(bundle.is_some());
//         let bundle = bundle.unwrap();
//
//         // The bundler should prefer smaller bundles since adding more random data increases gas per byte
//         assert_eq!(bundle.block_heights, 1..=1); // Only the first block included
//         assert!(bundle.optimal);
//
//         Ok(())
//     }
//
//     #[tokio::test]
//     async fn bundler_includes_more_random_data_when_overhead_reduces_per_byte_cost() -> Result<()> {
//         // Given an overhead threshold and overhead gas, including more data can reduce per-byte gas cost
//         let overhead_threshold = 1500; // If data size exceeds 1500 bytes, overhead applies
//         let overhead_gas = 1000; // Additional gas cost when overhead applies
//
//         let l1_adapter = MockL1Adapter {
//             gas_prices: GasPrices {
//                 storage: 1,
//                 normal: 1,
//             },
//             gas_usage_per_byte: 1,
//             max_bytes_per_submission: NonZeroUsize::new(5000).unwrap(),
//             overhead_threshold,
//             overhead_gas,
//         };
//         let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());
//
//         // Random data (not compressible)
//         use rand::{RngCore, SeedableRng};
//         let mut rng = rand::rngs::StdRng::seed_from_u64(42);
//
//         let block1_data: Vec<u8> = (0..1000).map(|_| rng.next_u32() as u8).collect();
//         let block2_data: Vec<u8> = (0..600).map(|_| rng.next_u32() as u8).collect();
//         let block3_data: Vec<u8> = (0..600).map(|_| rng.next_u32() as u8).collect();
//
//         let sequence = non_empty_vec![
//             FuelBlock {
//                 hash: [0; 32],
//                 height: 1,
//                 data: block1_data.try_into().unwrap(),
//             },
//             FuelBlock {
//                 hash: [1; 32],
//                 height: 2,
//                 data: block2_data.try_into().unwrap(),
//             },
//             FuelBlock {
//                 hash: [2; 32],
//                 height: 3,
//                 data: block3_data.try_into().unwrap(),
//             }
//         ]
//         .try_into()
//         .unwrap();
//
//         let mut bundler = factory.build(sequence).await;
//
//         // When
//         while bundler.advance().await? {}
//         let bundle = bundler.finish().await?;
//
//         // Then
//         assert!(bundle.is_some());
//         let bundle = bundle.unwrap();
//
//         // Since adding more data reduces overhead per byte, the bundler should include more blocks
//         // The combined size exceeds the overhead threshold, but including more data reduces per-byte cost
//         assert_eq!(bundle.block_heights, 1..=3);
//         assert!(bundle.optimal);
//
//         Ok(())
//     }
//
//     #[tokio::test]
//     async fn bundler_handles_thresholds_and_overheads_similar_to_eip_4844() -> Result<()> {
//         // Simulate behavior similar to EIP-4844 blobs
//         // - Up to 4096 bytes: pay for one blob
//         // - Every additional 4096 bytes: pay for another blob
//         // - After 6 blobs, additional overhead applies (e.g., another transaction fee)
//
//         // For simplicity, we'll define:
//         // - Blob size: 4096 bytes
//         // - Blob gas cost: 1000 gas per blob
//         // - Additional overhead after 6 blobs: 5000 gas
//
//         const BLOB_SIZE: usize = 4096;
//         const BLOB_GAS_COST: u64 = 1000;
//         const MAX_BLOBS_BEFORE_OVERHEAD: usize = 6;
//         const ADDITIONAL_OVERHEAD_GAS: u64 = 5000;
//
//         struct EIP4844MockL1Adapter {
//             gas_prices: GasPrices,
//             max_bytes_per_submission: NonZeroUsize,
//         }
//
//         #[async_trait::async_trait]
//         impl L1Api for EIP4844MockL1Adapter {
//             async fn gas_prices(&self) -> ports::l1::Result<GasPrices> {
//                 Ok(self.gas_prices)
//             }
//
//             fn gas_usage_to_store_data(&self, data_size: NonZeroUsize) -> GasUsage {
//                 let num_blobs = (data_size.get() + BLOB_SIZE - 1) / BLOB_SIZE; // Ceiling division
//                 let mut storage_gas = (num_blobs as u64) * BLOB_GAS_COST;
//
//                 if num_blobs > MAX_BLOBS_BEFORE_OVERHEAD {
//                     storage_gas += ADDITIONAL_OVERHEAD_GAS;
//                 }
//
//                 GasUsage {
//                     storage: storage_gas,
//                     normal: 0,
//                 }
//             }
//
//             fn max_bytes_per_submission(&self) -> NonZeroUsize {
//                 self.max_bytes_per_submission
//             }
//
//             async fn submit_l2_state(
//                 &self,
//                 state_data: NonEmptyVec<u8>,
//             ) -> ports::l1::Result<[u8; 32]> {
//                 unimplemented!()
//             }
//             async fn get_block_number(&self) -> ports::l1::Result<L1Height> {
//                 unimplemented!()
//             }
//             async fn balance(&self) -> ports::l1::Result<U256> {
//                 unimplemented!()
//             }
//             async fn get_transaction_response(
//                 &self,
//                 tx_hash: [u8; 32],
//             ) -> ports::l1::Result<Option<TransactionResponse>> {
//                 unimplemented!()
//             }
//         }
//
//         let l1_adapter = EIP4844MockL1Adapter {
//             gas_prices: GasPrices {
//                 storage: 1,
//                 normal: 1,
//             },
//             max_bytes_per_submission: NonZeroUsize::new(BLOB_SIZE * 10).unwrap(), // Arbitrary large limit
//         };
//         let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());
//
//         // Create blocks with data sizes that cross the blob thresholds
//         let block_data = vec![0u8; 1000]; // Highly compressible data
//         let blocks: Vec<FuelBlock> = (0..10)
//             .map(|i| FuelBlock {
//                 hash: [i as u8; 32],
//                 height: i as u32 + 1,
//                 data: block_data.clone().try_into().unwrap(),
//             })
//             .collect();
//
//         let sequence = NonEmptyVec::try_from(blocks).unwrap().try_into().unwrap();
//         let mut bundler = factory.build(sequence).await;
//
//         // When
//         while bundler.advance().await? {}
//         let bundle = bundler.finish().await?;
//
//         // Then
//         assert!(bundle.is_some());
//         let bundle = bundle.unwrap();
//
//         // The bundler should consider the overhead after 6 blobs and decide whether including more data is beneficial
//         // Since the data is highly compressible, including more data may not cross the blob thresholds due to compression
//
//         // Assuming compression keeps the compressed size within one blob, the bundler should include all blocks
//         assert!(*bundle.block_heights.end() >= 6); // Should include at least 6 blocks
//         assert!(bundle.optimal);
//
//         Ok(())
//     }
//
//     #[tokio::test]
//     async fn bundler_selects_optimal_bundle_based_on_overhead_and_data_size() -> Result<()> {
//         // Given
//         let overhead_threshold = 2000; // Overhead applies after 2000 bytes
//         let overhead_gas = 500; // Additional gas when overhead applies
//
//         let l1_adapter = MockL1Adapter {
//             gas_prices: GasPrices {
//                 storage: 1,
//                 normal: 1,
//             },
//             gas_usage_per_byte: 2, // Higher gas per byte
//             max_bytes_per_submission: NonZeroUsize::new(5000).unwrap(),
//             overhead_threshold,
//             overhead_gas,
//         };
//         let factory = Factory::new(Arc::new(l1_adapter), Compressor::default());
//
//         // First block is compressible, next blocks are random
//         let compressible_data = vec![0u8; 1500];
//         use rand::{RngCore, SeedableRng};
//         let mut rng = rand::rngs::StdRng::seed_from_u64(42);
//         let random_data: Vec<u8> = (0..600).map(|_| rng.next_u32() as u8).collect();
//
//         let sequence = non_empty_vec![
//             FuelBlock {
//                 hash: [0; 32],
//                 height: 1,
//                 data: compressible_data.clone().try_into().unwrap(),
//             },
//             FuelBlock {
//                 hash: [1; 32],
//                 height: 2,
//                 data: random_data.clone().try_into().unwrap(),
//             },
//             FuelBlock {
//                 hash: [2; 32],
//                 height: 3,
//                 data: random_data.clone().try_into().unwrap(),
//             }
//         ]
//         .try_into()
//         .unwrap();
//
//         let mut bundler = factory.build(sequence).await;
//
//         // When
//         while bundler.advance().await? {}
//         let bundle = bundler.finish().await?;
//
//         // Then
//         assert!(bundle.is_some());
//         let bundle = bundle.unwrap();
//
//         // The bundler should include all blocks if the overhead per byte is reduced by adding more data
//         assert_eq!(bundle.block_heights, 1..=3);
//         assert!(bundle.optimal);
//
//         Ok(())
//     }
// }
