use std::num::{NonZeroU32, NonZeroUsize};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use fuel_block_committer_encoding::bundle::{self, CompressionLevel};
use itertools::Itertools;
use rand::{Rng, SeedableRng, rngs::SmallRng};
use services::{
    Bundle,
    block_bundler::{common::BundlerFactory, eigen_bundler::Factory},
    types::{CompressedFuelBlock, NonEmpty, storage::SequentialFuelBlocks},
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

    // Create data with a mix of patterns and randomness to target ~2.2x compression
    let mut data = vec![0u8; size_bytes];

    // First create a few distinct patterns we'll reuse
    let num_patterns = 5;
    let pattern_size = 64;
    let mut patterns = Vec::with_capacity(num_patterns);

    for _ in 0..num_patterns {
        let mut pattern = vec![0u8; pattern_size];
        rng.fill(&mut pattern[..]);
        patterns.push(pattern);
    }

    // Apply the patterns to about 65% of the data
    // For 2.2x compression, we want a good mix of patterns and randomness
    for chunk in data.chunks_mut(128) {
        // First 64 bytes get a repeating pattern
        let pattern_idx = rng.r#gen_range(0..num_patterns);
        let pattern_bytes = chunk.len().min(pattern_size);

        if pattern_bytes > 0 {
            chunk[0..pattern_bytes].copy_from_slice(&patterns[pattern_idx][0..pattern_bytes]);
        }

        // Remaining bytes (if any) get random data
        if chunk.len() > pattern_bytes {
            rng.fill(&mut chunk[pattern_bytes..]);
        }
    }

    // Additional step: create some long-range patterns for better compression
    // Every 2KB, repeat a longer pattern
    let long_pattern_size = 256;
    let mut long_pattern = vec![0u8; long_pattern_size];
    rng.fill(&mut long_pattern[..]);

    for i in (0..size_bytes).step_by(2048) {
        let copy_size = (size_bytes - i).min(long_pattern_size);
        if copy_size > 0 {
            data[i..i + copy_size].copy_from_slice(&long_pattern[0..copy_size]);
        }
    }

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
