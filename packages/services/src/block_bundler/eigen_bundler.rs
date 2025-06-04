use std::num::{NonZeroU32, NonZeroUsize};

use fuel_block_committer_encoding::bundle;

use crate::{
    Result,
    block_bundler::common::{Bundle, BundleProposal, BundlerFactory, Metadata},
    types::{CompressedFuelBlock, Fragment, NonEmpty, NonNegative, storage::SequentialFuelBlocks},
};

pub struct Factory {
    fragment_size: NonZeroU32,
    max_fragments: NonZeroUsize,
    bundle_encoder: bundle::Encoder,
}

impl Factory {
    pub fn new(
        bundle_encoder: bundle::Encoder,
        fragment_size: NonZeroU32,
        max_fragments: NonZeroUsize,
    ) -> Self {
        Self {
            bundle_encoder,
            fragment_size,
            max_fragments,
        }
    }
}

impl BundlerFactory for Factory {
    type Bundler = EigenBundler;

    async fn build(&self, blocks: SequentialFuelBlocks, _id: NonNegative<i32>) -> Self::Bundler {
        EigenBundler::new(
            self.bundle_encoder.clone(),
            blocks,
            self.fragment_size,
            self.max_fragments,
        )
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
    max_fragments: NonZeroUsize,
    cached_bundle: Option<CompressedBundle>,
    cached_fragments: Option<NonEmpty<Fragment>>,
}

impl EigenBundler {
    pub fn new(
        bundle_encoder: bundle::Encoder,
        blocks: SequentialFuelBlocks,
        fragment_size: NonZeroU32,
        max_fragments: NonZeroUsize,
    ) -> Self {
        Self {
            bundle_encoder,
            blocks: blocks.into_inner(),
            fragment_size,
            max_fragments,
            cached_bundle: None,
            cached_fragments: None,
        }
    }

    fn create_fragments(
        fragment_size: NonZeroU32,
        compressed_data: Vec<u8>,
    ) -> Result<NonEmpty<Fragment>> {
        let fragments: Vec<Fragment> = compressed_data
            .chunks(fragment_size.get() as usize)
            .map(|chunk| Fragment {
                data: NonEmpty::from_vec(chunk.to_vec()).expect("chunk should not be empty"),
                unused_bytes: 0,
                total_bytes: fragment_size,
            })
            .collect();

        NonEmpty::from_vec(fragments)
            .ok_or_else(|| crate::Error::Other("no fragments created".to_string()))
    }

    fn trim_blocks_to_fit(
        &self,
    ) -> (
        NonEmpty<CompressedFuelBlock>,
        Option<CompressedBundle>,
        Option<NonEmpty<Fragment>>,
    ) {
        let mut current_blocks = self.blocks.clone();
        let original_count = current_blocks.len();
        let mut iterations = 0;

        tracing::info!(
            "EigenBundler: trim_blocks_to_fit starting with {} blocks",
            original_count
        );

        while let Ok(bundle) = {
            let encode_start = std::time::Instant::now();
            let result = encode_blocks(self.bundle_encoder.clone(), current_blocks.clone());

            if let Ok(ref bundle) = result {
                let encode_duration = encode_start.elapsed();
                iterations += 1;

                tracing::info!(
                    "EigenBundler: trim_blocks_to_fit iteration {}: encode_blocks took {:?} for {} blocks, produced {} bytes",
                    iterations,
                    encode_duration,
                    current_blocks.len(),
                    bundle.data.len()
                );
            }

            result
        } {
            if let Ok(fragments) = {
                let fragments_start = std::time::Instant::now();
                let result = Self::create_fragments(self.fragment_size, bundle.data.clone());

                if let Ok(ref fragments) = result {
                    let fragments_duration = fragments_start.elapsed();

                    tracing::info!(
                        "EigenBundler: trim_blocks_to_fit iteration {}: create_fragments took {:?}, created {} fragments",
                        iterations,
                        fragments_duration,
                        fragments.len()
                    );
                }

                result
            } {
                if fragments.len() <= self.max_fragments.get() {
                    tracing::info!(
                        "EigenBundler: trim_blocks_to_fit completed after {} iterations, using {} blocks (of original {})",
                        iterations,
                        current_blocks.len(),
                        original_count
                    );
                    return (current_blocks, Some(bundle), Some(fragments));
                }
            }

            current_blocks.pop();
            tracing::info!(
                "EigenBundler: trim_blocks_to_fit reduced blocks to {} (fragments > max_fragments: {})",
                current_blocks.len(),
                self.max_fragments.get()
            );
        }

        tracing::error!(
            "EigenBundler: Could not find a valid block bundle within fragment limit after {} iterations",
            iterations
        );
        panic!("Could not find a valid block bundle within fragment limit");
    }
}

impl Bundle for EigenBundler {
    async fn advance(&mut self, _num_concurrent: NonZeroUsize) -> Result<bool> {
        let start = std::time::Instant::now();

        // adjust blocks to fit within max_fragments constraint
        tracing::info!("EigenBundler: Starting trim_blocks_to_fit");
        let trim_start = std::time::Instant::now();
        let (new_blocks, bundle, fragments) = self.trim_blocks_to_fit();
        self.blocks = new_blocks;
        self.cached_bundle = bundle;
        self.cached_fragments = fragments;
        let trim_duration = trim_start.elapsed();
        tracing::info!("EigenBundler: trim_blocks_to_fit took {:?}", trim_duration);

        let total_duration = start.elapsed();
        tracing::info!("EigenBundler: advance() completed in {:?}", total_duration);
        Ok(true)
    }

    async fn finish(self) -> Result<BundleProposal> {
        let start = std::time::Instant::now();

        // Use cached results if available, otherwise perform encoding and fragmenting
        let (bundle, fragments) =
            if let (Some(bundle), Some(fragments)) = (self.cached_bundle, self.cached_fragments) {
                tracing::info!("EigenBundler: Using cached bundle and fragments from advance()");
                (bundle, fragments)
            } else {
                tracing::info!("EigenBundler: No cached data available, performing encoding");
                let encode_start = std::time::Instant::now();
                let bundle = encode_blocks(self.bundle_encoder.clone(), self.blocks.clone())?;
                let encode_duration = encode_start.elapsed();
                tracing::info!(
                    "EigenBundler: encode_blocks completed in {:?} for {} blocks, {:?} bytes",
                    encode_duration,
                    self.blocks.len(),
                    bundle.data.len()
                );

                let fragments_start = std::time::Instant::now();
                let fragments = Self::create_fragments(self.fragment_size, bundle.data.clone())?;
                let fragments_duration = fragments_start.elapsed();
                tracing::info!(
                    "EigenBundler: create_fragments completed in {:?}, created {} fragments",
                    fragments_duration,
                    fragments.len()
                );

                (bundle, fragments)
            };

        let uncompressed_data_size = NonZeroUsize::new(bundle.uncompressed_size)
            .expect("at least one block should be present");

        let compressed_data_size = NonZeroUsize::new(bundle.data.len())
            .ok_or_else(|| crate::Error::Other("compressed data is empty".to_string()))?;

        let total_duration = start.elapsed();
        tracing::info!("EigenBundler: finish() completed in {:?}", total_duration);

        Ok(BundleProposal {
            metadata: Metadata {
                block_heights: bundle.block_heights,
                compressed_data_size,
                uncompressed_data_size,
                num_fragments: fragments.len_nonzero(),
                known_to_be_optimal: true,
                optimization_attempts: 1,
                gas_usage: 0,
                block_num_upper_limit: NonZeroUsize::new(self.blocks.len())
                    .expect("valid block count"),
            },
            fragments,
        })
    }
}

fn encode_blocks(
    bundle_encoder: bundle::Encoder,
    bundle_blocks: NonEmpty<CompressedFuelBlock>,
) -> Result<CompressedBundle> {
    let start = std::time::Instant::now();

    let uncompressed_size = bundle_blocks.iter().map(|block| block.data.len()).sum();
    let block_heights = bundle_blocks.first().height..=bundle_blocks.last().height;

    let blocks_time = std::time::Instant::now();
    let blocks: Vec<Vec<u8>> = bundle_blocks
        .into_iter()
        .map(|block| Vec::from(block.data))
        .collect();
    let blocks_conversion_time = blocks_time.elapsed();

    tracing::info!(
        "encode_blocks: Converted {} blocks to Vec<u8> in {:?}",
        blocks.len(),
        blocks_conversion_time
    );

    let encode_time = std::time::Instant::now();
    let data = bundle_encoder
        .encode(bundle::Bundle::V1(bundle::BundleV1 { blocks }))
        .map_err(|e| crate::Error::Other(e.to_string()))?;
    let encode_duration = encode_time.elapsed();

    tracing::info!(
        "encode_blocks: Encoded data from {} bytes to {} bytes in {:?} (ratio: {:.2}x)",
        uncompressed_size,
        data.len(),
        encode_duration,
        uncompressed_size as f64 / data.len() as f64
    );

    let total_duration = start.elapsed();
    tracing::info!("encode_blocks: Total time: {:?}", total_duration);

    Ok(CompressedBundle {
        uncompressed_size,
        data,
        block_heights,
    })
}
