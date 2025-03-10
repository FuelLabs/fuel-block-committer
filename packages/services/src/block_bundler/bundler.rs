use std::{cmp::min, collections::VecDeque, num::NonZeroUsize};

use fuel_block_committer_encoding::bundle::{self, BundleV1};
use itertools::Itertools;
use rayon::prelude::*;

use crate::{
    types::{
        storage::SequentialFuelBlocks, CollectNonEmpty, CompressedFuelBlock, NonEmpty, NonNegative,
    },
    Result,
    types::{
        CollectNonEmpty, CompressedFuelBlock, Fragment, NonEmpty, NonNegative,
        storage::SequentialFuelBlocks,
    },
};

use super::{
    common::{Bundle, BundleProposal, BundlerFactory, Metadata},
    port::l1::FragmentEncoder,
};

pub struct Factory<GasCalculator> {
    gas_calc: GasCalculator,
    bundle_encoder: bundle::Encoder,
    step_size: NonZeroUsize,
    max_fragments: NonZeroUsize,
}

impl<L1> Factory<L1> {
    pub fn new(
        gas_calc: L1,
        bundle_encoder: bundle::Encoder,
        step_size: NonZeroUsize,
        max_fragments: NonZeroUsize,
    ) -> Self {
        Self {
            gas_calc,
            bundle_encoder,
            step_size,
            max_fragments,
        }
    }
}

impl<GasCalculator> BundlerFactory for Factory<GasCalculator>
where
    GasCalculator: FragmentEncoder + Clone + Send + Sync + 'static,
{
    type Bundler = Bundler<GasCalculator>;

    async fn build(&self, blocks: SequentialFuelBlocks, id: NonNegative<i32>) -> Self::Bundler {
        Bundler::new(
            self.gas_calc.clone(),
            blocks,
            self.bundle_encoder.clone(),
            self.step_size,
            id,
            self.max_fragments,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Proposal {
    block_heights: std::ops::RangeInclusive<u32>,
    uncompressed_data_size: NonZeroUsize,
    compressed_data: NonEmpty<u8>,
    gas_usage: u64,
    num_fragments: NonZeroUsize,
}

impl Proposal {
    fn block_count(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.block_heights.clone().count()).expect("not empty range")
    }

    fn gas_per_uncompressed_byte(&self) -> f64 {
        self.gas_usage as f64 / self.uncompressed_data_size.get() as f64
    }
}

#[derive(Debug, Clone)]
pub struct Bundler<FragmentEncoder> {
    fragment_encoder: FragmentEncoder,
    blocks: NonEmpty<CompressedFuelBlock>,
    best_valid_proposal: Option<Proposal>,
    smallest_invalid_proposal: Option<Proposal>,
    bundle_encoder: bundle::Encoder,
    sizes_to_try: VecDeque<NonZeroUsize>,
    attempts_made: usize,
    max_blocks_in_bundle: NonZeroUsize,
    bundle_id: NonNegative<i32>,
    max_fragments: NonZeroUsize,
}

impl<T> Bundler<T> {
    pub fn new(
        cost_calculator: T,
        blocks: SequentialFuelBlocks,
        bundle_encoder: bundle::Encoder,
        initial_step_size: NonZeroUsize,
        bundle_id: NonNegative<i32>,
        max_fragments: NonZeroUsize,
    ) -> Self {
        let max_blocks = blocks.len();
        let attempts = generate_attempts(max_blocks, initial_step_size);

        Self {
            fragment_encoder: cost_calculator,
            blocks: blocks.into_inner(),
            best_valid_proposal: None,
            smallest_invalid_proposal: None,
            bundle_encoder,
            sizes_to_try: attempts,
            bundle_id,
            max_fragments,
            attempts_made: 0,
            max_blocks_in_bundle: max_blocks,
        }
    }

    fn save_if_best_so_far(&mut self, new_proposal: Proposal) {
        if new_proposal.num_fragments <= self.max_fragments {
            match &mut self.best_valid_proposal {
                Some(best)
                    if new_proposal.gas_per_uncompressed_byte()
                        < best.gas_per_uncompressed_byte() =>
                {
                    *best = new_proposal;
                }
                None => {
                    self.best_valid_proposal = Some(new_proposal);
                }
                _ => {}
            }
        } else {
            let block_count_failed = new_proposal.block_count();
            match &mut self.smallest_invalid_proposal {
                Some(smallest)
                    if new_proposal.compressed_data.len() < smallest.compressed_data.len() =>
                {
                    *smallest = new_proposal;
                }
                None => {
                    self.smallest_invalid_proposal = Some(new_proposal);
                }
                _ => {}
            }

            let min_blocks_in_bundle = NonZeroUsize::try_from(1).unwrap();
            let max_blocks_in_bundle =
                NonZeroUsize::try_from(block_count_failed.get().saturating_sub(1))
                    .unwrap_or(min_blocks_in_bundle);

            self.max_blocks_in_bundle = min(self.max_blocks_in_bundle, max_blocks_in_bundle);
        }
    }

    fn blocks_for_new_proposal(&self, block_count: NonZeroUsize) -> NonEmpty<CompressedFuelBlock> {
        self.blocks
            .iter()
            .take(block_count.get())
            .cloned()
            .collect_nonempty()
            .expect("non-empty")
    }

    fn blocks_bundles_for_analyzing(
        &mut self,
        num_concurrent: std::num::NonZeroUsize,
    ) -> Vec<NonEmpty<CompressedFuelBlock>> {
        let mut blocks_for_attempts = vec![];

        while !self.sizes_to_try.is_empty() && blocks_for_attempts.len() < num_concurrent.get() {
            let block_count = self.sizes_to_try.pop_front().expect("not empty");
            let blocks = self.blocks_for_new_proposal(block_count);
            blocks_for_attempts.push(blocks);
        }
        blocks_for_attempts
    }

    async fn analyze(&mut self, num_concurrent: std::num::NonZeroUsize) -> Result<Vec<Proposal>>
    where
        T: FragmentEncoder + Send + Sync + Clone + 'static,
    {
        let blocks_for_analyzing = self.blocks_bundles_for_analyzing(num_concurrent);

        let bundle_encoder = self.bundle_encoder.clone();
        let fragment_encoder = self.fragment_encoder.clone();

        // Offload to a blocking thread so as not to block the async runtime
        tokio::task::spawn_blocking(move || {
            blocks_for_analyzing
                .into_par_iter()
                .map(|blocks| {
                    let fragment_encoder = fragment_encoder.clone();
                    let bundle_encoder = bundle_encoder.clone();
                    create_proposal(bundle_encoder, fragment_encoder, blocks)
                })
                .collect::<Result<Vec<_>>>()
        })
        .await
        .map_err(|e| crate::Error::Other(e.to_string()))?
    }
}

impl<T> Bundle for Bundler<T>
where
    T: FragmentEncoder + Send + Sync + Clone + 'static,
{
    async fn advance(&mut self, optimization_runs: NonZeroUsize) -> Result<bool> {
        self.sizes_to_try
            .retain(|size| size <= &self.max_blocks_in_bundle);

        if self.sizes_to_try.is_empty() {
            return Ok(false);
        }
        let proposals = self.analyze(optimization_runs).await?;
        self.attempts_made += proposals.len();

        for proposal in proposals {
            self.save_if_best_so_far(proposal);
        }

        Ok(!self.sizes_to_try.is_empty())
    }

    async fn finish(mut self) -> Result<BundleProposal> {
        if self.best_valid_proposal.is_none() {
            self.advance(NonZeroUsize::new(1).unwrap()).await?;
        }

        let best_proposal = self
            .best_valid_proposal
            .take()
            .or(self.smallest_invalid_proposal.take())
            .expect("advance should have set at least one");

        let compressed_data_size = best_proposal.compressed_data.len_nonzero();
        let fragments = self
            .fragment_encoder
            .encode(best_proposal.compressed_data, self.bundle_id)?;

        Ok(BundleProposal {
            metadata: Metadata {
                block_heights: best_proposal.block_heights,
                known_to_be_optimal: self.sizes_to_try.is_empty(),
                uncompressed_data_size: best_proposal.uncompressed_data_size,
                compressed_data_size,
                gas_usage: best_proposal.gas_usage,
                optimization_attempts: self.attempts_made,
                num_fragments: fragments.len_nonzero(),
                block_num_upper_limit: self.max_blocks_in_bundle,
            },
            fragments,
        })
    }
}

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

fn create_proposal(
    bundle_encoder: bundle::Encoder,
    fragment_encoder: impl FragmentEncoder,
    bundle_blocks: NonEmpty<CompressedFuelBlock>,
) -> Result<Proposal> {
    let block_heights = bundle_blocks.first().height..=bundle_blocks.last().height;

    let blocks: Vec<Vec<u8>> = bundle_blocks
        .into_iter()
        .map(|block| Vec::from(block.data))
        .collect();

    let uncompressed_data_size =
        NonZeroUsize::try_from(blocks.iter().map(|b| b.len()).sum::<usize>())
            .expect("sum of block sizes should be > 0");

    let compressed_data = bundle_encoder
        .encode(bundle::Bundle::V1(BundleV1 { blocks }))
        .map_err(|e| crate::Error::Other(e.to_string()))?;

    let compressed_data = NonEmpty::from_vec(compressed_data)
        .ok_or_else(|| crate::Error::Other("bundle encoder returned zero bytes".to_string()))?;

    let num_fragments = fragment_encoder.num_fragments_needed(compressed_data.len_nonzero());
    let gas_usage = fragment_encoder.gas_usage(compressed_data.len_nonzero());

    Ok(Proposal {
        block_heights,
        uncompressed_data_size,
        compressed_data,
        gas_usage,
        num_fragments,
    })
}
