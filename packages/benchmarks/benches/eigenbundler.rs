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
use tracing_subscriber::{fmt, EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

// Specific case configuration
const TOTAL_DATA_SIZE_MB: usize = 28;
const BLOCK_SIZE_MB: usize = 4;
const FRAGMENT_SIZE_MB: f64 = 3.5;
const MAX_FRAGMENTS: usize = 12;

// Calculate total number of blocks
const NUM_BLOCKS: usize = TOTAL_DATA_SIZE_MB.div_ceil(BLOCK_SIZE_MB); // Round up

// Initialize logging for the benchmark
fn init_logging() {
    // Only initialize once
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // Set up logging similar to production
        tracing_subscriber::registry()
            .with(fmt::layer().with_target(true))
            .with(EnvFilter::from_default_env()
                 .add_directive(tracing::Level::INFO.into())
                 .add_directive("services::block_bundler=info".parse().unwrap())
                 .add_directive("fuel_block_committer_encoding=info".parse().unwrap()))
            .init();
        
        tracing::info!("Benchmark logging initialized");
    });
}

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

// Run a single bundling operation with full logging
fn run_full_bundling_with_logs() {
    // Initialize logging
    init_logging();
    
    // Set up the runtime
    let rt = tokio::runtime::Runtime::new().unwrap();
    
    // Run the benchmark with full logging
    rt.block_on(async {
        tracing::info!("Starting benchmark with full logging");
        
        // Create test blocks
        let blocks = create_test_blocks();
        let num_blocks = blocks.len();
        
        tracing::info!("Created {} test blocks with total size {}MB", 
                      num_blocks, TOTAL_DATA_SIZE_MB);
        
        // Create bundler
        let fragment_size_bytes = (3_500_000) as u32;
        let fragment_size = NonZeroU32::new(fragment_size_bytes).unwrap();
        let max_fragments = NonZeroUsize::new(MAX_FRAGMENTS).unwrap();
        let compression_level = CompressionLevel::Level6;
        
        let bundler_factory = Factory::new(
            bundle::Encoder::new(compression_level),
            fragment_size,
            max_fragments,
        );
        
        tracing::info!("Created bundler factory with fragment_size={} bytes, max_fragments={}",
                      fragment_size.get(), max_fragments.get());
        
        // Build the bundler
        let bundler = bundler_factory.build(blocks, 0.into()).await;
        let mut bundler = bundler;
        
        tracing::info!("Built bundler, starting processing");
        
        // First stage: advance
        let advance_start = Instant::now();
        tracing::info!("Starting advance() operation");
        let _ = bundler.advance(NonZeroUsize::new(1).unwrap()).await.unwrap();
        let advance_time = advance_start.elapsed();
        tracing::info!("Advance operation completed in {:?}", advance_time);
        
        // Second stage: finish
        let finish_start = Instant::now();
        tracing::info!("Starting finish() operation");
        let result = bundler.finish().await.unwrap();
        let finish_time = finish_start.elapsed();
        tracing::info!("Finish operation completed in {:?}", finish_time);
        
        // Report results
        let compression_ratio = result.metadata.compression_ratio();
        tracing::info!(
            "Bundling complete: Metadata: blocks={}, compression_ratio={:.2}x, fragments={}, advance_time={:?}, finish_time={:?}",
            num_blocks,
            compression_ratio,
            result.metadata.num_fragments,
            advance_time,
            finish_time
        );
    });
}

// Benchmark the specific case
fn bench_specific_case(c: &mut Criterion) {
    let mut group = c.benchmark_group("eigenbundler_specific_case");

    // Set throughput to measure blocks per second instead of bytes
    group.throughput(Throughput::Elements(NUM_BLOCKS as u64));

    // Convert fragment size to bytes and ensure it's non-zero
    let fragment_size_bytes = (FRAGMENT_SIZE_MB * 1024.0 * 1024.0) as u32;
    let fragment_size = NonZeroU32::new(fragment_size_bytes).unwrap();
    let max_fragments = NonZeroUsize::new(MAX_FRAGMENTS).unwrap();

    // Only test compression level 6
    let compression_level = CompressionLevel::Level6;

    // Benchmark both bundling stages to match production behavior
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

                    // First stage: advance (which calls trim_blocks_to_fit())
                    let advance_start = Instant::now();
                    let _ = bundler
                        .advance(NonZeroUsize::new(1).unwrap())
                        .await
                        .unwrap();
                    let advance_time = advance_start.elapsed();

                    // Second stage: finish (which encodes again)
                    let finish_start = Instant::now();
                    let result = bundler.finish().await.unwrap();
                    let finish_time = finish_start.elapsed();

                    let elapsed = start.elapsed();

                    // Calculate metrics
                    let compression_ratio = result.metadata.compression_ratio();
                    let time_per_block = elapsed.as_millis() as f64 / num_blocks.get() as f64;

                    println!(
                        "Compression ratio: {compression_ratio:.2}x, Processing rate: {time_per_block:.2}ms/block, Advance: {:?}, Finish: {:?}, Total: {:?}",
                        advance_time,
                        finish_time,
                        elapsed
                    );

                    // Return the bundle result
                    criterion::black_box((result, compression_ratio, time_per_block))
                })
            },
        );
    });

    group.finish();
}

fn main() {
    // Run once with full logging enabled
    run_full_bundling_with_logs();
    
    // Then run the actual benchmarks
    criterion_main!(benches);
}

criterion_group!(benches, bench_specific_case);
