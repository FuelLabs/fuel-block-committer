use std::num::{NonZeroU32, NonZeroUsize};

use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group,
    criterion_main, measurement::WallTime,
};
use fuel_block_committer_encoding::bundle::{self, BundleV1, CompressionLevel};
use itertools::Itertools;
use rand::{Rng, SeedableRng, rngs::SmallRng};
use services::{
    Bundle,
    block_bundler::{
        common::BundlerFactory,
        eigen_bundler::{self, EigenBundler, Factory},
    },
    types::{CompressedFuelBlock, NonEmpty, nonempty, storage::SequentialFuelBlocks},
};
use tokio::time::Instant;

// Specific case configuration
const TOTAL_DATA_SIZE_MB: usize = 28;
const BLOCK_SIZE_MB: usize = 4;
const FRAGMENT_SIZE_MB: f64 = 3.5;
const MAX_FRAGMENTS: usize = 12;

// Calculate total number of blocks
const NUM_BLOCKS: usize = TOTAL_DATA_SIZE_MB.div_ceil(BLOCK_SIZE_MB); // Round up

// Generate data that achieves approximately a 2.2x compression ratio
fn generate_compressible_data(size_bytes: usize) -> Vec<u8> {
    let mut rng = SmallRng::seed_from_u64(42); // Fixed seed for reproducibility

    // Create a base pattern that can be repeated
    let pattern_size = 64;
    let mut base_pattern = vec![0u8; pattern_size];
    rng.fill(&mut base_pattern[..]);

    // For a compression ratio of about 2.2x, we can use a pattern but
    // introduce some controlled randomness
    let mut data = Vec::with_capacity(size_bytes);

    let mut counter = 0;
    while data.len() < size_bytes {
        // Add the base pattern
        data.extend_from_slice(&base_pattern);

        // Every few iterations, add some semi-random data to reduce compressibility
        // This helps achieve closer to 2.2x rather than higher ratios
        counter += 1;
        if counter % 5 == 0 {
            for _ in 0..16 {
                data.push(rng.r#gen::<u8>());
            }
        }
    }

    data.truncate(size_bytes);
    data
}

// Create a block with the given block number and data
fn create_block(height: u32, data: Vec<u8>) -> CompressedFuelBlock {
    CompressedFuelBlock {
        height,
        data: NonEmpty::from_vec(data).unwrap(),
    }
}

// Create the test blocks for our specific case
fn create_test_blocks() -> SequentialFuelBlocks {
    let block_size_bytes = BLOCK_SIZE_MB * 1024 * 1024;

    let blocks = (0..NUM_BLOCKS)
        .map(|i| {
            let data = generate_compressible_data(block_size_bytes);
            create_block(i as u32, data)
        })
        .collect_vec();

    let non_empty_blocks = NonEmpty::from_vec(blocks).expect("Should have at least one block");

    SequentialFuelBlocks::try_from(non_empty_blocks).expect("Blocks should be sequential")
}

// Benchmark the specific case
fn bench_specific_case(c: &mut Criterion) {
    // Configure benchmark with fewer samples
    let mut group = c.benchmark_group("eigenbundler_specific_case");

    // Reduce warm-up time
    group.warm_up_time(std::time::Duration::from_millis(500));

    // Set throughput to measure blocks per second instead of bytes
    group.throughput(Throughput::Elements(NUM_BLOCKS as u64));

    // Convert fragment size to bytes and ensure it's non-zero
    let fragment_size_bytes = (FRAGMENT_SIZE_MB * 1024.0 * 1024.0) as u32;
    let fragment_size = NonZeroU32::new(fragment_size_bytes).unwrap();
    let max_fragments = NonZeroUsize::new(MAX_FRAGMENTS).unwrap();

    // Only test compression level 6
    let compression_level = CompressionLevel::Level6;

    let id = BenchmarkId::new(
        format!("{}MB_data_{}blocks", TOTAL_DATA_SIZE_MB, NUM_BLOCKS),
        "Level6",
    );

    group.bench_with_input(id, &NUM_BLOCKS, |b, &_| {
        b.iter_with_setup(
            || {
                // Setup: create test blocks and bundler
                let blocks = create_test_blocks();

                let bundler_factory = Factory::new(
                    bundle::Encoder::new(compression_level),
                    fragment_size,
                    max_fragments,
                );

                (blocks, bundler_factory)
            },
            |(blocks, factory)| {
                // Benchmark the full bundling process using a tokio runtime
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    let num_blocks = blocks.len();
                    let bundler = factory.build(blocks, 0.into()).await;
                    let mut bundler = bundler;

                    let start = Instant::now();
                    // Advance until complete
                    let _ = bundler
                        .advance(NonZeroUsize::new(1).unwrap())
                        .await
                        .unwrap();

                    // Finish and produce final bundle
                    let result = bundler.finish().await.unwrap();
                    let elapsed = start.elapsed();

                    // Calculate metrics
                    let compression_ratio = result.metadata.compression_ratio();

                    let bundle_time_ms = elapsed;

                    let time_per_block = bundle_time_ms.as_millis() as f64 / num_blocks.get() as f64;

                    println!(
                        "Compression ratio: {compression_ratio:.2}x, Processing rate: {time_per_block:.2}ms/block",
                    );

                    // Return the bundle result
                    criterion::black_box((result, compression_ratio, time_per_block))
                })
            },
        );
    });

    group.finish();
}

criterion_group!(benches, bench_specific_case);
criterion_main!(benches);
