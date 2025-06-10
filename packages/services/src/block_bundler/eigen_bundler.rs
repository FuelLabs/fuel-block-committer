use std::num::{NonZeroU32, NonZeroUsize};

use fuel_block_committer_encoding::bundle;

use crate::{
    Result,
    block_bundler::common::{Bundle, BundleProposal, BundlerFactory, Metadata},
    types::{CompressedFuelBlock, Fragment, NonEmpty, NonNegative, storage::SequentialFuelBlocks},
};

/// Factory for creating instances of `EigenBundler` with specific configurations.
pub struct Factory<E> {
    /// The size of each fragment in bytes
    fragment_size: NonZeroU32,
    /// The maximum number of fragments allowed in a bundle
    max_fragments: NonZeroUsize,
    /// The encoder used to create the bundle
    bundle_encoder: E,
}

impl<E> Factory<E> {
    pub fn new(bundle_encoder: E, fragment_size: NonZeroU32, max_fragments: NonZeroUsize) -> Self {
        Self {
            bundle_encoder,
            fragment_size,
            max_fragments,
        }
    }
}

impl<E> BundlerFactory for Factory<E>
where
    E: bundle::BundleEncoder + Clone + Send + Sync,
{
    type Bundler = EigenBundler<E>;

    async fn build(&self, blocks: SequentialFuelBlocks, _id: NonNegative<i32>) -> Self::Bundler {
        EigenBundler::<E>::new(
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

/// Bundler that encodes fuel blocks into a compressed bundle and fragments it
pub struct EigenBundler<E> {
    /// The encoder used to create the bundle
    bundle_encoder: E,
    /// The blocks to be bundled
    blocks: NonEmpty<CompressedFuelBlock>,
    /// The size of each fragment in bytes
    fragment_size: NonZeroU32,
    /// The maximum number of fragments allowed in a bundle
    max_fragments: NonZeroUsize,
    /// Cached bundle from the last `advance` call
    cached_bundle: Option<CompressedBundle>,
    /// Cached fragments from the last `advance` call
    cached_fragments: Option<NonEmpty<Fragment>>,
}

impl<E> EigenBundler<E> {
    /// Creates a new instance of `EigenBundler`
    fn new(
        bundle_encoder: E,
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
}

type TrimmedResult = Result<(
    NonEmpty<CompressedFuelBlock>,
    Option<CompressedBundle>,
    Option<NonEmpty<Fragment>>,
)>;

impl<E> EigenBundler<E>
where
    E: bundle::BundleEncoder + Clone,
{
    fn create_fragments(
        fragment_size: NonZeroU32,
        compressed_data: Vec<u8>,
    ) -> Result<NonEmpty<Fragment>> {
        let fragments: Vec<Fragment> = compressed_data
            .chunks(fragment_size.get() as usize)
            .map(|chunk| Fragment {
                data: NonEmpty::from_vec(chunk.to_vec()).expect("chunk should not be empty"),
                unused_bytes: (fragment_size.get() as usize - chunk.len()) as u32,
                total_bytes: NonZeroU32::new(chunk.len() as u32).unwrap(),
            })
            .collect();

        NonEmpty::from_vec(fragments)
            .ok_or_else(|| crate::Error::Bundler("no fragments created".to_string()))
    }

    fn trim_blocks_to_fit(&self) -> TrimmedResult {
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
                    return Ok((current_blocks, Some(bundle), Some(fragments)));
                }
            }

            if current_blocks.len() == 1 {
                tracing::error!(
                    "EigenBundler: Could not find a valid block bundle within fragment limit after {} iterations",
                    iterations
                );
                return Err(crate::Error::Bundler(
                    "Could not find a valid block bundle within fragment limit".to_string(),
                ));
            }

            current_blocks.pop();
            tracing::info!(
                "EigenBundler: trim_blocks_to_fit reduced blocks to {} (fragments > max_fragments: {})",
                current_blocks.len(),
                self.max_fragments.get()
            );
        }

        tracing::error!(
            "EigenBundler: Could not find and encode a valid block bundle within fragment limit after {} iterations",
            iterations
        );
        Err(crate::Error::Bundler(
            "Could not find and encode a valid block bundle within fragment limit".to_string(),
        ))
    }
}

impl<E> Bundle for EigenBundler<E>
where
    E: bundle::BundleEncoder + Clone + Send + Sync,
{
    async fn advance(&mut self, _num_concurrent: NonZeroUsize) -> Result<bool> {
        let start = std::time::Instant::now();

        // adjust blocks to fit within max_fragments constraint
        tracing::info!("EigenBundler: Starting trim_blocks_to_fit");
        let trim_start = std::time::Instant::now();
        let (new_blocks, bundle, fragments) = self.trim_blocks_to_fit()?;
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
            .ok_or_else(|| crate::Error::Bundler("compressed data is empty".to_string()))?;

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

fn encode_blocks<E: bundle::BundleEncoder>(
    bundle_encoder: E,
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
        .map_err(|e| crate::Error::Bundler(e.to_string()))?;
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

#[allow(non_snake_case)]
#[cfg(test)]
mod tests {
    use super::*;

    // Factory tests
    #[test]
    fn factory__new__creates_with_correct_fields() {
        // given
        let bundle_encoder = bundle::Encoder::default();
        let fragment_size = NonZeroU32::new(1024).unwrap();
        let max_fragments = NonZeroUsize::new(10).unwrap();

        // when
        let factory = Factory::new(bundle_encoder, fragment_size, max_fragments);

        // then
        assert_eq!(factory.fragment_size, fragment_size);
        assert_eq!(factory.max_fragments, max_fragments);
    }

    #[tokio::test]
    async fn factory__build__creates_eigen_bundler() {
        // given
        let bundle_encoder = bundle::Encoder::default();
        let fragment_size = NonZeroU32::new(1024).unwrap();
        let max_fragments = NonZeroUsize::new(10).unwrap();
        let factory = Factory::new(bundle_encoder.clone(), fragment_size, max_fragments);

        let test_block = CompressedFuelBlock {
            height: 1,
            data: NonEmpty::from_vec(vec![1, 2, 3, 4]).unwrap(),
        };
        let blocks = NonEmpty::from_vec(vec![test_block]).unwrap();
        let id = 42.try_into().unwrap();

        // when
        let bundler = factory.build(blocks.clone().try_into().unwrap(), id).await;

        // then
        assert_eq!(bundler.fragment_size, fragment_size);
        assert_eq!(bundler.max_fragments, max_fragments);
        assert_eq!(bundler.blocks.len(), 1);
        assert_eq!(bundler.blocks.first().height, 1);
        assert!(bundler.cached_bundle.is_none());
        assert!(bundler.cached_fragments.is_none());
    }

    // EigenBundler Bundle trait tests
    #[tokio::test]
    async fn eigen_bundler__advance__caches_bundle_and_fragments() {
        // given
        let bundle_encoder = bundle::Encoder::default();
        let fragment_size = NonZeroU32::new(1024).unwrap();
        let max_fragments = NonZeroUsize::new(10).unwrap();

        let test_block = CompressedFuelBlock {
            height: 1,
            data: NonEmpty::from_vec(vec![1, 2, 3, 4, 5]).unwrap(),
        };
        let sequential_blocks =
            SequentialFuelBlocks::new(NonEmpty::from_vec(vec![test_block]).unwrap()).unwrap();

        let mut bundler = EigenBundler::new(
            bundle_encoder,
            sequential_blocks,
            fragment_size,
            max_fragments,
        );

        // when
        let advanced = bundler
            .advance(NonZeroUsize::new(1).unwrap())
            .await
            .unwrap();

        // then
        assert!(advanced);
        assert!(bundler.cached_bundle.is_some());
        assert!(bundler.cached_fragments.is_some());

        // Verify cached data contains expected information
        let cached_bundle = bundler.cached_bundle.as_ref().unwrap();
        let cached_fragments = bundler.cached_fragments.as_ref().unwrap();

        assert_eq!(cached_bundle.block_heights, 1..=1);
        assert!(!cached_bundle.data.is_empty());
        assert!(!cached_fragments.is_empty());
    }

    #[tokio::test]
    async fn eigen_bundler__advance__trims_blocks_to_fit_fragment_limit() {
        // given
        let bundle_encoder = bundle::Encoder::default();
        let fragment_size = NonZeroU32::new(100).unwrap(); // Small fragment size
        let max_fragments = NonZeroUsize::new(2).unwrap(); // Very low fragment limit

        let test_blocks: Vec<CompressedFuelBlock> = (1..=5)
            .map(|height| CompressedFuelBlock {
                height,
                data: NonEmpty::from_vec(vec![height as u8; 500]).unwrap(), // Large blocks
            })
            .collect();

        let sequential_blocks =
            SequentialFuelBlocks::new(NonEmpty::from_vec(test_blocks).unwrap()).unwrap();

        let mut bundler = EigenBundler::new(
            bundle_encoder,
            sequential_blocks,
            fragment_size,
            max_fragments,
        );

        let initial_block_count = bundler.blocks.len();
        assert_eq!(initial_block_count, 5);

        // when
        let _ = bundler
            .advance(NonZeroUsize::new(1).unwrap())
            .await
            .unwrap();

        // then

        let final_block_count = bundler.blocks.len();
        assert!(final_block_count <= initial_block_count);
        assert!(final_block_count > 0); // Should still have at least one block
    }

    #[tokio::test]
    async fn eigen_bundler__finish__uses_cached_data_when_available() {
        // given
        let bundle_encoder = bundle::Encoder::default();
        let fragment_size = NonZeroU32::new(1024).unwrap();
        let max_fragments = NonZeroUsize::new(10).unwrap();

        let test_block = CompressedFuelBlock {
            height: 42,
            data: NonEmpty::from_vec(vec![1, 2, 3, 4, 5]).unwrap(),
        };
        let sequential_blocks =
            SequentialFuelBlocks::new(NonEmpty::from_vec(vec![test_block]).unwrap()).unwrap();

        let mut bundler = EigenBundler::new(
            bundle_encoder,
            sequential_blocks,
            fragment_size,
            max_fragments,
        );

        // Populate cache by calling advance
        let advance_result = bundler.advance(NonZeroUsize::new(1).unwrap()).await;
        assert!(advance_result.is_ok());
        assert!(bundler.cached_bundle.is_some());
        assert!(bundler.cached_fragments.is_some());

        // when
        let bundle_proposal = bundler.finish().await.unwrap();

        // then
        assert_eq!(bundle_proposal.metadata.block_heights, 42..=42);
        assert!(bundle_proposal.metadata.compressed_data_size.get() > 0);
        assert!(bundle_proposal.metadata.uncompressed_data_size.get() > 0);
        assert_eq!(
            bundle_proposal.metadata.num_fragments.get(),
            bundle_proposal.fragments.len()
        );
        assert!(bundle_proposal.metadata.known_to_be_optimal);
        assert_eq!(bundle_proposal.metadata.optimization_attempts, 1);
        assert_eq!(bundle_proposal.metadata.gas_usage, 0);
        assert_eq!(bundle_proposal.metadata.block_num_upper_limit.get(), 1);
    }

    #[tokio::test]
    async fn eigen_bundler__finish__encodes_when_no_cached_data() {
        // given
        let bundle_encoder = bundle::Encoder::default();
        let fragment_size = NonZeroU32::new(1024).unwrap();
        let max_fragments = NonZeroUsize::new(10).unwrap();

        let test_block = CompressedFuelBlock {
            height: 42,
            data: NonEmpty::from_vec(vec![1, 2, 3, 4, 5]).unwrap(),
        };
        let sequential_blocks =
            SequentialFuelBlocks::new(NonEmpty::from_vec(vec![test_block]).unwrap()).unwrap();

        let bundler = EigenBundler::new(
            bundle_encoder,
            sequential_blocks,
            fragment_size,
            max_fragments,
        );

        // when
        let bundle_proposal = bundler.finish().await.unwrap();

        // then
        assert_eq!(bundle_proposal.metadata.block_heights, 42..=42);
        assert!(bundle_proposal.metadata.compressed_data_size.get() > 0);
        assert!(bundle_proposal.metadata.uncompressed_data_size.get() > 0);
        assert_eq!(
            bundle_proposal.metadata.num_fragments.get(),
            bundle_proposal.fragments.len()
        );
        assert!(bundle_proposal.metadata.known_to_be_optimal);
        assert_eq!(bundle_proposal.metadata.optimization_attempts, 1);
        assert_eq!(bundle_proposal.metadata.gas_usage, 0);
        assert_eq!(bundle_proposal.metadata.block_num_upper_limit.get(), 1);
    }

    #[tokio::test]
    async fn eigen_bundler__finish__returns_correct_bundle_proposal() {
        // given
        let bundle_encoder = bundle::Encoder::default();
        let fragment_size = NonZeroU32::new(512).unwrap();
        let max_fragments = NonZeroUsize::new(5).unwrap();

        let test_blocks = vec![
            CompressedFuelBlock {
                height: 10,
                data: NonEmpty::from_vec(vec![1; 100]).unwrap(),
            },
            CompressedFuelBlock {
                height: 11,
                data: NonEmpty::from_vec(vec![2; 150]).unwrap(),
            },
            CompressedFuelBlock {
                height: 12,
                data: NonEmpty::from_vec(vec![3; 200]).unwrap(),
            },
        ];
        let expected_uncompressed_size = 100 + 150 + 200; // 450 bytes
        let sequential_blocks =
            SequentialFuelBlocks::new(NonEmpty::from_vec(test_blocks).unwrap()).unwrap();

        let bundler = EigenBundler::new(
            bundle_encoder,
            sequential_blocks,
            fragment_size,
            max_fragments,
        );

        // when
        let bundle_proposal = bundler.finish().await.unwrap();

        // then
        // Verify metadata correctness
        let metadata = &bundle_proposal.metadata;
        assert_eq!(metadata.block_heights, 10..=12);
        assert_eq!(
            metadata.uncompressed_data_size.get(),
            expected_uncompressed_size
        );
        assert!(metadata.compressed_data_size.get() > 0);
        assert!(metadata.compressed_data_size.get() <= expected_uncompressed_size); // Should be compressed
        assert_eq!(
            metadata.num_fragments.get(),
            bundle_proposal.fragments.len()
        );
        assert!(metadata.known_to_be_optimal);
        assert_eq!(metadata.optimization_attempts, 1);
        assert_eq!(metadata.gas_usage, 0);
        assert_eq!(metadata.block_num_upper_limit.get(), 3);

        // Verify fragments structure
        assert!(!bundle_proposal.fragments.is_empty());
        for fragment in bundle_proposal.fragments.iter() {
            assert!(!fragment.data.is_empty());
            assert_eq!(fragment.total_bytes, fragment_size);
            assert_eq!(fragment.unused_bytes, 0);
        }

        // Verify fragment count calculation
        let expected_fragment_count = bundle_proposal.fragments.len();
        assert_eq!(metadata.num_fragments.get(), expected_fragment_count);
    }

    #[tokio::test]
    async fn eigen_bundler__advance__errors_when_single_block_exceeds_fragment_limit() {
        // given
        let bundle_encoder = bundle::Encoder::default();
        let fragment_size = NonZeroU32::new(10).unwrap(); // Very small fragments
        let max_fragments = NonZeroUsize::new(1).unwrap(); // Only allow 1 fragment

        let large_data = vec![42u8; 1000]; // 1000 bytes of data
        let test_block = CompressedFuelBlock {
            height: 1,
            data: NonEmpty::from_vec(large_data).unwrap(),
        };
        let sequential_blocks =
            SequentialFuelBlocks::new(NonEmpty::from_vec(vec![test_block]).unwrap()).unwrap();

        let mut bundler = EigenBundler::new(
            bundle_encoder,
            sequential_blocks,
            fragment_size,
            max_fragments,
        );

        // when - this should error because even 1 block creates > 1 fragment
        let result = bundler.advance(NonZeroUsize::new(1).unwrap()).await;

        // then
        assert!(result.is_err());
    }

    #[derive(Clone)]
    struct FakeBundleEncoder;

    #[derive(derive_more::Error, derive_more::Display, Debug)]
    enum FakeBundleEncoderError {
        EncodingError,
        CompressionError,
    }

    impl bundle::BundleEncoder for FakeBundleEncoder {
        type Error = FakeBundleEncoderError;

        fn encode(&self, _bundle: bundle::Bundle) -> std::result::Result<Vec<u8>, Self::Error> {
            Err(Self::Error::EncodingError)
        }

        fn compress(&self, _data: &[u8]) -> std::result::Result<Vec<u8>, Self::Error> {
            Err(Self::Error::CompressionError)
        }
    }

    #[tokio::test]
    async fn eigen_bundle__advance__errors_when_encoding_fails() {
        // given
        let bundle_encoder = FakeBundleEncoder;
        let fragment_size = NonZeroU32::new(1024).unwrap();
        let max_fragments = NonZeroUsize::new(10).unwrap();

        let test_block = CompressedFuelBlock {
            height: 1,
            data: NonEmpty::from_vec(vec![1, 2, 3, 4, 5]).unwrap(),
        };
        let sequential_blocks =
            SequentialFuelBlocks::new(NonEmpty::from_vec(vec![test_block]).unwrap()).unwrap();

        let mut bundler = EigenBundler::new(
            bundle_encoder,
            sequential_blocks,
            fragment_size,
            max_fragments,
        );

        // when - this should error because FakeBundleEncoder does not support encoding
        let result = bundler.advance(NonZeroUsize::new(1).unwrap()).await;

        // then
        assert!(result.is_err());
    }
}
