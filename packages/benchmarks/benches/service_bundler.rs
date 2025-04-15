use std::{
    num::{NonZeroU32, NonZeroUsize},
    ops::RangeInclusive,
    sync::Arc,
    time::Duration,
};

use clock::{SystemClock};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use fuel_block_committer_encoding::bundle::{self, CompressionLevel};
use itertools::Itertools;
use rand::{Rng, SeedableRng, rngs::SmallRng};
use services::{
    BlockBundler, BlockBundlerConfig, Runner,
    block_bundler::{
        common::BundlerFactory,
        eigen_bundler::Factory,
        port::{
            self,
            UnbundledBlocks,
        }
    },
    types::{
        CompressedFuelBlock, 
        Fragment, 
        NonEmpty, 
        NonNegative,
        storage::SequentialFuelBlocks,
    },
};
use tokio::sync::Mutex;
use tracing_subscriber::{fmt, EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

// Specific case configuration
const TOTAL_DATA_SIZE_MB: usize = 28;
const BLOCK_SIZE_MB: usize = 4;
const FRAGMENT_SIZE_MB: f64 = 3.5;
const MAX_FRAGMENTS: usize = 12;

// Calculate total number of blocks
const NUM_BLOCKS: usize = (TOTAL_DATA_SIZE_MB + BLOCK_SIZE_MB - 1) / BLOCK_SIZE_MB; // Round up

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
                 .add_directive("services::block_bundler::eigen_bundler=info".parse().unwrap())
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
        let pattern_idx = rng.gen_range(0..num_patterns);
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
fn create_test_blocks(range: RangeInclusive<u32>) -> Vec<CompressedFuelBlock> {
    let block_size_bytes = BLOCK_SIZE_MB * 1024 * 1024;
    
    range.map(|i| {
        let data = generate_compressible_data(block_size_bytes);
        create_block(i, data)
    }).collect_vec()
}

// In-memory implementation of the Fuel API
struct MockFuelApi {
    latest_height: u32,
}

impl port::fuel::Api for MockFuelApi {
    async fn latest_height(&self) -> services::Result<u32> {
        Ok(self.latest_height)
    }
}

// In-memory implementation of the Storage port
#[derive(Clone)]
struct InMemoryStorage {
    blocks: Arc<Mutex<Vec<CompressedFuelBlock>>>,
    next_id: Arc<Mutex<i32>>,
    fragments: Arc<Mutex<Vec<(NonNegative<i32>, RangeInclusive<u32>, NonEmpty<Fragment>)>>>,
}

impl InMemoryStorage {
    fn new(blocks: Vec<CompressedFuelBlock>) -> Self {
        Self {
            blocks: Arc::new(Mutex::new(blocks)),
            next_id: Arc::new(Mutex::new(0)),
            fragments: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl port::Storage for InMemoryStorage {
    async fn lowest_sequence_of_unbundled_blocks(
        &self,
        starting_height: u32,
        max_cumulative_bytes: u32,
    ) -> services::Result<Option<UnbundledBlocks>> {
        let blocks = self.blocks.lock().await;
        
        if blocks.is_empty() {
            return Ok(None);
        }
        
        // Filter blocks by starting height
        let eligible_blocks: Vec<CompressedFuelBlock> = blocks
            .iter()
            .filter(|b| b.height >= starting_height)
            .cloned()
            .collect();
        
        if eligible_blocks.is_empty() {
            return Ok(None);
        }
        
        // Sort blocks by height
        let mut sorted_blocks = eligible_blocks.clone();
        sorted_blocks.sort_by_key(|b| b.height);
        
        // Find a continuous sequence
        let mut sequence = Vec::new();
        let mut cumulative_size = 0;
        let mut current_height = sorted_blocks[0].height;
        
        for block in sorted_blocks {
            if block.height != current_height {
                break;
            }
            
            let block_size = block.data.len();
            if cumulative_size + block_size <= max_cumulative_bytes as usize {
                sequence.push(block);
                cumulative_size += block_size;
                current_height += 1;
            } else {
                break;
            }
        }
        
        if sequence.is_empty() {
            return Ok(None);
        }
        
        let non_empty_seq = NonEmpty::from_vec(sequence).unwrap();
        let seq_blocks = SequentialFuelBlocks::try_from(non_empty_seq).unwrap();
        
        Ok(Some(UnbundledBlocks {
            oldest: seq_blocks,
            total_unbundled: NonZeroUsize::new(eligible_blocks.len()).unwrap(),
        }))
    }
    
    async fn insert_bundle_and_fragments(
        &self,
        bundle_id: NonNegative<i32>,
        block_range: RangeInclusive<u32>,
        fragments: NonEmpty<Fragment>,
    ) -> services::Result<()> {
        // Mark blocks as bundled by removing them
        let mut blocks = self.blocks.lock().await;
        blocks.retain(|b| !block_range.contains(&b.height));
        
        // Store fragments
        let mut stored_fragments = self.fragments.lock().await;
        stored_fragments.push((bundle_id, block_range, fragments));
        
        Ok(())
    }
    
    async fn next_bundle_id(&self) -> services::Result<NonNegative<i32>> {
        let mut id = self.next_id.lock().await;
        let current = *id;
        *id += 1;
        Ok(current.try_into().unwrap())
    }
}

// Run a single full service bundling operation with logging
fn run_service_bundling_with_logs() {
    // Initialize logging
    init_logging();
    
    // Set up the runtime
    let rt = tokio::runtime::Runtime::new().unwrap();
    
    // Run the full service benchmark with logging
    rt.block_on(async {
        tracing::info!("Starting full service benchmark with logging");
        
        // Create test blocks
        let block_range = 0..=(NUM_BLOCKS as u32 - 1);
        let blocks = create_test_blocks(block_range.clone());
        
        tracing::info!("Created {} test blocks with total size {}MB", 
                      blocks.len(), TOTAL_DATA_SIZE_MB);
        
        // Create the in-memory storage
        let storage = InMemoryStorage::new(blocks);
        
        // Create the mock Fuel API
        let fuel_api = MockFuelApi { latest_height: *block_range.end() };
        
        // Create bundler factory
        let fragment_size_bytes = 3_500_000; // 3.5MB
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
        
        // Create service config
        let config = BlockBundlerConfig {
            optimization_time_limit: Duration::from_secs(4), // Same as production
            max_bundles_per_optimization_run: NonZeroUsize::new(1).unwrap(),
            max_fragments_per_bundle: max_fragments,
            accumulation_time_limit: Duration::from_secs(1),
            bytes_to_accumulate: NonZeroUsize::new(TOTAL_DATA_SIZE_MB * 1024 * 1024).unwrap(),
            blocks_to_accumulate: NonZeroUsize::new(1).unwrap(), // Start bundling immediately
            lookback_window: 1000,
        };
        
        // Create the block bundler service
        let clock = SystemClock;
        let mut block_bundler = BlockBundler::new(
            fuel_api,
            storage.clone(),
            clock,
            bundler_factory,
            config,
        );
        
        tracing::info!("Created BlockBundler service, starting run");
        
        // Start the benchmark timer
        let start = std::time::Instant::now();
        
        // Run the service
        block_bundler.run().await.unwrap();
        
        // End the benchmark timer
        let elapsed = start.elapsed();
        
        // Check if all blocks were bundled
        let remaining_blocks = storage.blocks.lock().await.len();
        let fragments = storage.fragments.lock().await.clone();
        
        tracing::info!(
            "BlockBundler service run completed in {:?}. Remaining blocks: {}, Created fragment bundles: {}",
            elapsed,
            remaining_blocks,
            fragments.len()
        );
        
        // If fragments were created, print details about the first bundle
        if !fragments.is_empty() {
            let (bundle_id, block_range, fragments) = &fragments[0];
            tracing::info!(
                "First bundle: id={}, blocks={:?}, fragments={}, first fragment size={}",
                bundle_id,
                block_range,
                fragments.len(),
                fragments.first().data.len()
            );
        }
    });
}

fn bench_service_bundler(c: &mut Criterion) {
    let mut group = c.benchmark_group("service_bundler");
    
    // Benchmark ID
    let id = BenchmarkId::new(
        format!("{}MB_data_{}blocks", TOTAL_DATA_SIZE_MB, NUM_BLOCKS), 
        "Level6",
    );
    
    group.bench_function(id, |b| {
        b.iter_with_setup(
            || {
                // Setup: create test blocks and services
                let block_range = 0..=(NUM_BLOCKS as u32 - 1);
                let blocks = create_test_blocks(block_range.clone());
                
                // Create the in-memory storage
                let storage = InMemoryStorage::new(blocks);
                
                // Create the mock Fuel API
                let fuel_api = MockFuelApi { latest_height: *block_range.end() };
                
                // Create bundler factory
                let fragment_size_bytes = 3_500_000; // 3.5MB
                let fragment_size = NonZeroU32::new(fragment_size_bytes).unwrap();
                let max_fragments = NonZeroUsize::new(MAX_FRAGMENTS).unwrap();
                let compression_level = CompressionLevel::Level6;
                
                let bundler_factory = Factory::new(
                    bundle::Encoder::new(compression_level),
                    fragment_size,
                    max_fragments,
                );
                
                // Create service config
                let config = BlockBundlerConfig {
                    optimization_time_limit: Duration::from_secs(4), // Same as production
                    max_bundles_per_optimization_run: NonZeroUsize::new(1).unwrap(),
                    max_fragments_per_bundle: max_fragments,
                    accumulation_time_limit: Duration::from_secs(1),
                    bytes_to_accumulate: NonZeroUsize::new(TOTAL_DATA_SIZE_MB * 1024 * 1024).unwrap(),
                    blocks_to_accumulate: NonZeroUsize::new(1).unwrap(), // Start bundling immediately
                    lookback_window: 1000,
                };
                
                // Create the block bundler service
                let clock = SystemClock;
                let mut block_bundler = BlockBundler::new(
                    fuel_api,
                    storage.clone(),
                    clock,
                    bundler_factory,
                    config,
                );
                
                block_bundler
            },
            |mut block_bundler| {
                // Run the service once
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    block_bundler.run().await.unwrap();
                });
            },
        );
    });
    
    group.finish();
}

fn main() {
    // Run once with full logging enabled
    run_service_bundling_with_logs();
    
    // Then run the actual benchmarks
    criterion_main!(benches);
}

criterion_group!(benches, bench_service_bundler); 
