use itertools::Itertools;
use ports::storage::ValidatedRange;

use crate::Result;

use super::{Bundle, BundleProposal, BundlerFactory};

pub struct Factory<L1, Storage> {
    l1: L1,
    storage: Storage,
    acceptable_amount_of_blocks: ValidatedRange<usize>,
}

impl<L1, Storage> Factory<L1, Storage> {
    pub fn new(
        l1: L1,
        storage: Storage,
        acceptable_amount_of_blocks: ValidatedRange<usize>,
    ) -> Self {
        Self {
            acceptable_amount_of_blocks,
            l1,
            storage,
        }
    }
}

#[async_trait::async_trait]
impl<L1, Storage> BundlerFactory for Factory<L1, Storage>
where
    Bundler<L1>: Bundle,
    Storage: ports::storage::Storage + 'static,
    L1: Send + Sync + 'static + Clone,
{
    type Bundler = Bundler<L1>;
    async fn build(&self) -> Result<Self::Bundler> {
        let max_blocks = self
            .acceptable_amount_of_blocks
            .inner()
            .end
            .saturating_sub(1);
        let blocks = self.storage.lowest_unbundled_blocks(max_blocks).await?;

        Ok(Bundler::new(
            self.l1.clone(),
            blocks,
            self.acceptable_amount_of_blocks.clone(),
        ))
    }
}

pub struct Bundler<L1> {
    l1_adapter: L1,
    blocks: Vec<ports::storage::FuelBlock>,
    acceptable_amount_of_blocks: ValidatedRange<usize>,
    best_run: Option<BundleProposal>,
    next_block_amount: Option<usize>,
}

impl<L1> Bundler<L1> {
    pub fn new(
        l1_adapter: L1,
        blocks: Vec<ports::storage::FuelBlock>,
        acceptable_amount_of_blocks: ValidatedRange<usize>,
    ) -> Self {
        Self {
            l1_adapter,
            blocks,
            acceptable_amount_of_blocks,
            best_run: None,
            next_block_amount: None,
        }
    }
}

#[async_trait::async_trait]
impl<L1> Bundle for Bundler<L1>
where
    L1: ports::l1::Api + Send + Sync,
{
    async fn propose_bundle(&mut self) -> Result<Option<BundleProposal>> {
        if self.blocks.is_empty() {
            return Ok(None);
        }

        let min_possible_blocks = self
            .acceptable_amount_of_blocks
            .inner()
            .clone()
            .min()
            .unwrap();

        let max_possible_blocks = self
            .acceptable_amount_of_blocks
            .inner()
            .clone()
            .max()
            .unwrap();

        if self.blocks.len() < min_possible_blocks {
            return Ok(None);
        }

        let amount_of_blocks_to_try = self.next_block_amount.unwrap_or(min_possible_blocks);

        let merged_data = self.blocks[..amount_of_blocks_to_try]
            .iter()
            .flat_map(|b| b.data.clone().into_inner())
            .collect::<Vec<_>>();

        let submittable_chunks = self
            .l1_adapter
            .split_into_submittable_fragments(&merged_data.try_into().expect("cannot be empty"))?;

        let fragments = submittable_chunks;

        let (min_height, max_height) = self.blocks.as_slice()[..amount_of_blocks_to_try]
            .iter()
            .map(|b| b.height)
            .minmax()
            .into_option()
            .unwrap();

        let block_heights = (min_height..max_height + 1).try_into().unwrap();

        match &mut self.best_run {
            None => {
                self.best_run = Some(BundleProposal {
                    fragments,
                    block_heights,
                    optimal: false,
                });
            }
            Some(best_run) => {
                if best_run.fragments.gas_estimation >= fragments.gas_estimation {
                    self.best_run = Some(BundleProposal {
                        fragments,
                        block_heights,
                        optimal: false,
                    });
                }
            }
        }

        let last_try = amount_of_blocks_to_try == max_possible_blocks;

        let best = self.best_run.as_ref().unwrap().clone();

        self.next_block_amount = Some(amount_of_blocks_to_try.saturating_add(1));

        Ok(Some(BundleProposal {
            optimal: last_try,
            ..best
        }))
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
