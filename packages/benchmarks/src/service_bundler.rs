use std::{
    num::{NonZeroU32, NonZeroUsize},
    ops::RangeInclusive,
    sync::Arc,
    time::Duration,
};

use clock::SystemClock;
use fuel_block_committer_encoding::bundle::{self, CompressionLevel};
use itertools::Itertools;
use rand::{Rng, RngCore, SeedableRng, rngs::SmallRng};
use services::{
    BlockBundler, BlockBundlerConfig, Runner,
    block_bundler::{
        eigen_bundler::Factory,
        port::{self, UnbundledBlocks},
    },
    types::{CompressedFuelBlock, Fragment, NonEmpty, NonNegative, storage::SequentialFuelBlocks},
};
use tokio::sync::Mutex;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

// Specific case configuration
const TOTAL_DATA_SIZE_MB: usize = 28;
const BLOCK_SIZE_MB: usize = 4;
const FRAGMENT_SIZE_MB: f64 = 3.5;
const MAX_FRAGMENTS: usize = 12;

// Calculate total number of blocks
const NUM_BLOCKS: usize = TOTAL_DATA_SIZE_MB.div_ceil(BLOCK_SIZE_MB); // Round up

// Compressibility enum matching the e2e test
#[allow(dead_code)]
#[derive(Debug, Clone)]
enum Compressibility {
    Random,
    Low,
    Medium,
    High,
    Full,
}

// Initialize logging for the benchmark
fn init_logging() {
    // Only initialize once
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // Set up logging similar to production
        tracing_subscriber::registry()
            .with(fmt::layer().with_target(true))
            .with(
                EnvFilter::from_default_env()
                    .add_directive(tracing::Level::INFO.into())
                    .add_directive("services::block_bundler=info".parse().unwrap())
                    .add_directive(
                        "services::block_bundler::eigen_bundler=info"
                            .parse()
                            .unwrap(),
                    )
                    .add_directive("fuel_block_committer_encoding=info".parse().unwrap()),
            )
            .init();

        tracing::info!("Benchmark logging initialized");
    });
}

// Generate block contents using the same algorithm as in e2e tests
fn generate_block_contents(block_size: usize, compressibility: &Compressibility) -> Vec<u8> {
    let mut rng = SmallRng::seed_from_u64(42); // Use fixed seed for reproducibility
    let mut block = vec![0u8; block_size];

    match compressibility {
        Compressibility::Random => {
            rng.fill_bytes(&mut block);
        }
        Compressibility::Full => {
            // All bytes remain zero.
        }
        Compressibility::Low => {
            rng.fill_bytes(&mut block);
            for byte in block.iter_mut() {
                if rng.gen_bool(0.25) {
                    *byte = 0;
                }
            }
        }
        Compressibility::Medium => {
            rng.fill_bytes(&mut block);
            for byte in block.iter_mut() {
                if rng.gen_bool(0.50) {
                    *byte = 0;
                }
            }
        }
        Compressibility::High => {
            rng.fill_bytes(&mut block);
            for byte in block.iter_mut() {
                if rng.gen_bool(0.75) {
                    *byte = 0;
                }
            }
        }
    }

    block
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

    range
        .map(|i| {
            // Use High compressibility to match ~2.2x compression factor
            let data = generate_block_contents(block_size_bytes, &Compressibility::High);
            create_block(i, data)
        })
        .collect_vec()
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

#[tokio::main]
async fn main() {
    println!("\n===== SERVICE BUNDLER BENCHMARK =====\n");

    // Initialize logging
    init_logging();

    // Print test configuration
    println!("\nTest configuration:");
    println!("- Total data size: {} MB", TOTAL_DATA_SIZE_MB);
    println!("- Block size: {} MB", BLOCK_SIZE_MB);
    println!("- Number of blocks: {}", NUM_BLOCKS);
    println!("- Fragment size: {:.1} MB", FRAGMENT_SIZE_MB);
    println!("- Max fragments: {}", MAX_FRAGMENTS);
    println!("- Compressibility: High (75% zeros)");
    println!();

    tracing::info!("Starting service bundler benchmark");

    // Create test blocks
    let block_range = 0..=(NUM_BLOCKS as u32 - 1);
    let blocks = create_test_blocks(block_range.clone());

    tracing::info!(
        "Created {} test blocks with total size {}MB",
        blocks.len(),
        TOTAL_DATA_SIZE_MB
    );

    // Create the in-memory storage
    let storage = InMemoryStorage::new(blocks);

    // Create the mock Fuel API
    let fuel_api = MockFuelApi {
        latest_height: *block_range.end(),
    };

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

    tracing::info!(
        "Created bundler factory with fragment_size={} bytes, max_fragments={}",
        fragment_size.get(),
        max_fragments.get()
    );

    let config = BlockBundlerConfig {
        optimization_time_limit: Duration::from_secs(0),
        max_bundles_per_optimization_run: NonZeroUsize::new(1).unwrap(),
        max_fragments_per_bundle: max_fragments,
        accumulation_time_limit: Duration::from_secs(1),
        bytes_to_accumulate: NonZeroUsize::new(TOTAL_DATA_SIZE_MB * 1024 * 1024).unwrap(),
        blocks_to_accumulate: NonZeroUsize::new(3600).unwrap(),
        lookback_window: 1000,
    };

    // Create the block bundler service
    let clock = SystemClock;

    let mut block_bundler =
        BlockBundler::new(fuel_api, storage.clone(), clock, bundler_factory, config);

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
        let (bundle_id, block_range, fragments) = &fragments.last().unwrap();
        tracing::info!(
            "Bundle: id={}, blocks={:?}, fragments={}, first fragment size={}",
            bundle_id,
            block_range,
            fragments.len(),
            fragments.first().data.len()
        );

        println!("\nBenchmark Results:");
        println!("- Total execution time: {:.2?}", elapsed);
        println!("- Bundled blocks: {:?}", block_range);
        println!("- Created fragments: {}", fragments.len());
        println!(
            "- Throughput: {:.2} blocks/second",
            (*block_range.end() - *block_range.start() + 1) as f64 / elapsed.as_secs_f64()
        );
    }

    println!("\n===== BENCHMARK COMPLETE =====\n");
}
