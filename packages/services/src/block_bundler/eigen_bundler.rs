use std::num::{NonZeroU32, NonZeroUsize};

use fuel_block_committer_encoding::bundle;

use crate::{
    block_bundler::common::{Bundle, BundleProposal, BundlerFactory, Metadata},
    types::{storage::SequentialFuelBlocks, CompressedFuelBlock, Fragment, NonEmpty, NonNegative},
    Result,
};

pub struct EigenFactory {
    fragment_size: NonZeroU32,
    bundle_encoder: bundle::Encoder,
}

impl EigenFactory {
    pub fn new(bundle_encoder: bundle::Encoder, fragment_size: NonZeroU32) -> Self {
        Self {
            bundle_encoder,
            fragment_size,
        }
    }
}

impl BundlerFactory for EigenFactory {
    type Bundler = EigenBundler;

    async fn build(&self, blocks: SequentialFuelBlocks, _id: NonNegative<i32>) -> Self::Bundler {
        EigenBundler::new(self.bundle_encoder.clone(), blocks, self.fragment_size)
    }
}

struct CompressedBundle {
    data: Vec<u8>,
    block_heights: std::ops::RangeInclusive<u32>,
    uncompressed_size: usize,
}

pub struct EigenBundler {
    bundle_encoder: bundle::Encoder,
    blocks: NonEmpty<CompressedFuelBlock>,
    fragment_size: NonZeroU32,
}

impl EigenBundler {
    pub fn new(
        bundle_encoder: bundle::Encoder,
        blocks: SequentialFuelBlocks,
        fragment_size: NonZeroU32,
    ) -> Self {
        Self {
            bundle_encoder,
            blocks: blocks.into_inner(),
            fragment_size,
        }
    }

    fn create_fragments(&self, compressed_data: Vec<u8>) -> Result<NonEmpty<Fragment>> {
        let fragments: Vec<Fragment> = compressed_data
            .chunks(self.fragment_size.get() as usize)
            .enumerate()
            .map(|(_, chunk)| Fragment {
                data: NonEmpty::from_vec(chunk.to_vec()).expect("chunk should not be empty"),
                unused_bytes: 0,
                total_bytes: self.fragment_size,
            })
            .collect();

        NonEmpty::from_vec(fragments)
            .ok_or_else(|| crate::Error::Other("no fragments created".to_string()))
    }
}

impl Bundle for EigenBundler {
    async fn advance(&mut self, _num_concurrent: NonZeroUsize) -> Result<bool> {
        // EigenDA bundler processes everything in one go
        Ok(true)
    }

    async fn finish(self) -> Result<BundleProposal> {
        let bundle = encode_blocks(self.bundle_encoder.clone(), self.blocks.clone())?;

        let uncompressed_data_size = NonZeroUsize::new(bundle.uncompressed_size)
            .expect("at least one block should be present");

        let compressed_data_size = NonZeroUsize::new(bundle.data.len())
            .ok_or_else(|| crate::Error::Other("compressed data is empty".to_string()))?;

        let fragments = self.create_fragments(bundle.data)?;

        Ok(BundleProposal {
            metadata: Metadata {
                block_heights: bundle.block_heights,
                compressed_data_size,
                uncompressed_data_size,
                num_fragments: fragments.len_nonzero(),
                // TODO: These fields can be ignored for now until we refactor the bundler
                known_to_be_optimal: true,
                optimization_attempts: 1,
                gas_usage: 0,
            },
            fragments,
        })
    }
}

fn encode_blocks(
    bundle_encoder: bundle::Encoder,
    bundle_blocks: NonEmpty<CompressedFuelBlock>,
) -> Result<CompressedBundle> {
    let uncompressed_size = bundle_blocks.iter().map(|block| block.data.len()).sum();
    let block_heights = bundle_blocks.first().height..=bundle_blocks.last().height;

    let blocks: Vec<Vec<u8>> = bundle_blocks
        .into_iter()
        .map(|block| Vec::from(block.data))
        .collect();

    let data = bundle_encoder
        .encode(bundle::Bundle::V1(bundle::BundleV1 { blocks }))
        .map_err(|e| crate::Error::Other(e.to_string()))?;

    Ok(CompressedBundle {
        uncompressed_size,
        data,
        block_heights,
    })
}
