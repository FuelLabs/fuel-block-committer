use itertools::Itertools;
use ports::{l1::SubmittableFragments, storage::ValidatedRange, types::NonEmptyVec};
use tracing::info;

use crate::Result;

use super::{Bundle, BundleProposal, BundlerFactory};

pub struct Factory<L1, Storage> {
    l1_adapter: L1,
    storage: Storage,
    acceptable_block_range: ValidatedRange<usize>,
}

impl<L1, Storage> Factory<L1, Storage> {
    pub fn new(
        l1_adapter: L1,
        storage: Storage,
        acceptable_block_range: ValidatedRange<usize>,
    ) -> Self {
        Self {
            l1_adapter,
            storage,
            acceptable_block_range,
        }
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
        let max_blocks = self.acceptable_block_range.inner().end.saturating_sub(1);
        let blocks = self.storage.lowest_unbundled_blocks(max_blocks).await?;

        Ok(Bundler::new(
            self.l1_adapter.clone(),
            blocks,
            self.acceptable_block_range.clone(),
        ))
    }
}

pub struct BestProposal {
    proposal: BundleProposal,
    gas_per_byte: f64,
    data_size: usize, // Uncompressed data size
}

pub struct Bundler<L1> {
    l1_adapter: L1,
    blocks: Vec<ports::storage::FuelBlock>,
    acceptable_block_range: ValidatedRange<usize>,
    best_proposal: Option<BestProposal>, // Refactored into BestProposal
    current_block_count: usize,
}

impl<L1> Bundler<L1> {
    pub fn new(
        l1_adapter: L1,
        blocks: Vec<ports::storage::FuelBlock>,
        acceptable_block_range: ValidatedRange<usize>,
    ) -> Self {
        let min_blocks = acceptable_block_range.inner().clone().min().unwrap_or(1);
        Self {
            l1_adapter,
            blocks,
            acceptable_block_range,
            best_proposal: None,
            current_block_count: min_blocks,
        }
    }

    /// Merges the data from the given blocks into a `NonEmptyVec<u8>`.
    fn merge_block_data(&self, block_slice: &[ports::storage::FuelBlock]) -> NonEmptyVec<u8> {
        let merged_data: Vec<u8> = block_slice
            .iter()
            .flat_map(|b| b.data.clone().into_inner())
            .collect();

        NonEmptyVec::try_from(merged_data).expect("Merged block data cannot be empty")
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

    /// Calculates the gas per byte ratio for uncompressed data.
    fn calculate_gas_per_byte(&self, gas_estimation: u128, data_size: usize) -> f64 {
        gas_estimation as f64 / data_size as f64
    }

    /// Determines if the current proposal is better based on gas per byte and data size.
    fn is_current_proposal_better(&self, gas_per_byte: f64, data_size: usize) -> bool {
        match &self.best_proposal {
            None => true, // No best proposal yet, so the current one is better
            Some(best_proposal) => {
                if gas_per_byte < best_proposal.gas_per_byte {
                    true // Current proposal has a better (lower) gas per byte ratio
                } else if gas_per_byte == best_proposal.gas_per_byte {
                    // If the gas per byte is the same, the proposal with more data is better
                    data_size > best_proposal.data_size
                } else {
                    false // Current proposal has a worse (higher) gas per byte ratio
                }
            }
        }
    }

    /// Updates the best proposal with the current proposal.
    fn update_best_proposal(
        &mut self,
        current_proposal: BundleProposal,
        gas_per_byte: f64,
        data_size: usize,
    ) {
        self.best_proposal = Some(BestProposal {
            proposal: current_proposal,
            gas_per_byte,
            data_size,
        });
    }
}

#[async_trait::async_trait]
impl<L1> Bundle for Bundler<L1>
where
    L1: ports::l1::Api + Send + Sync,
{
    async fn propose_bundle(&mut self) -> Result<Option<BundleProposal>> {
        if self.blocks.is_empty() {
            info!("No blocks available for bundling.");
            return Ok(None);
        }

        let min_blocks = self
            .acceptable_block_range
            .inner()
            .clone()
            .min()
            .unwrap_or(1);
        let max_blocks = self
            .acceptable_block_range
            .inner()
            .clone()
            .max()
            .unwrap_or(self.blocks.len());

        if self.blocks.len() < min_blocks {
            info!(
                "Not enough blocks to meet the minimum requirement: {}",
                min_blocks
            );
            return Ok(None);
        }

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

        let block_slice = &self.blocks[..self.current_block_count];

        // Merge block data
        let merged_data = self.merge_block_data(block_slice);

        // Split into submittable fragments
        let fragments = self
            .l1_adapter
            .split_into_submittable_fragments(&merged_data)?;

        // Extract block heights
        let block_heights = self.extract_block_heights(block_slice);

        // Calculate gas per byte ratio (based on uncompressed data)
        let data_size = merged_data.len();
        let gas_per_byte = self.calculate_gas_per_byte(fragments.gas_estimation, data_size);

        let current_proposal = BundleProposal {
            fragments,
            block_heights,
            optimal: false,
        };

        // Check if the current proposal is better
        if self.is_current_proposal_better(gas_per_byte, data_size) {
            self.update_best_proposal(current_proposal, gas_per_byte, data_size);
        }

        // Prepare for the next iteration
        self.current_block_count += 1;

        // Return the best proposal so far
        Ok(self.best_proposal.as_ref().map(|bp| bp.proposal.clone()))
    }
}

#[cfg(test)]
mod tests {
    use fuel_crypto::SecretKey;
    use itertools::Itertools;
    use ports::{l1::SubmittableFragments, non_empty_vec, types::NonEmptyVec};

    use crate::{
        state_committer::bundler::{gas_optimizing::Bundler, Bundle, BundleProposal},
        test_utils, Result,
    };

    #[tokio::test]
    async fn gas_optimizing_bundler_works_in_iterations() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = (0..=3)
            .map(|height| test_utils::mocks::fuel::generate_storage_block(height, &secret_key))
            .collect_vec();

        let bundle_of_blocks_0_and_1: NonEmptyVec<u8> = blocks[0..=1]
            .iter()
            .flat_map(|block| block.data.clone().into_inner())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        let fragment_of_unoptimal_block = test_utils::random_data(100);

        let l1_mock = test_utils::mocks::l1::will_split_bundles_into_fragments([(
            bundle_of_blocks_0_and_1.clone(),
            SubmittableFragments {
                fragments: non_empty_vec![fragment_of_unoptimal_block.clone()],
                gas_estimation: 100,
            },
        )]);

        let mut sut = Bundler::new(l1_mock, blocks, (2..4).try_into().unwrap());

        // when
        let bundle = sut.propose_bundle().await.unwrap().unwrap();

        // then
        assert_eq!(
            bundle,
            BundleProposal {
                fragments: SubmittableFragments {
                    fragments: non_empty_vec!(fragment_of_unoptimal_block),
                    gas_estimation: 100
                },
                block_heights: (0..2).try_into().unwrap(),
                optimal: false
            }
        );

        Ok(())
    }
}
