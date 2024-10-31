use std::{num::NonZeroUsize, time::Duration};

use clock::TestClock;
use eth::BlobEncoder;
use fuel_block_committer_encoding::bundle::{self, CompressionLevel};
use itertools::Itertools;
use metrics::RegistersMetrics;
use ports::{
    storage::{SequentialFuelBlocks, Storage},
    types::{nonempty, CollectNonEmpty, Fragment, NonEmpty},
};
use services::{
    BlockBundler, BlockBundlerConfig, Bundle, BundleProposal, Bundler, BundlerFactory,
    ControllableBundlerFactory, Metadata, Result, Runner,
};
use test_helpers::{
    bundle_and_encode_into_blobs,
    mocks::{
        self,
        fuel::{generate_block, generate_storage_block_sequence},
    },
    Blocks,
};

#[tokio::test]
async fn bundler_finishing_will_advance_if_not_called_at_least_once() {
    // given
    let blocks = generate_storage_block_sequence(0..=10, 1000);

    let bundler = Bundler::new(
        BlobEncoder,
        blocks.clone(),
        bundle::Encoder::new(CompressionLevel::Disabled),
        NonZeroUsize::new(1).unwrap(),
        1u16.into(),
    );

    // when
    let bundle = bundler.finish().await.unwrap();

    // then
    let expected_fragments = bundle_and_encode_into_blobs(blocks.into_inner(), 1);
    assert!(!bundle.metadata.known_to_be_optimal);
    assert_eq!(bundle.metadata.block_heights, 0..=10);
    assert_eq!(bundle.fragments, expected_fragments);
}

#[tokio::test]
async fn bundler_will_provide_a_suboptimal_bundle_if_not_advanced_enough() -> Result<()> {
    // given
    let stops_at_blob_boundary = generate_block(0, enough_bytes_to_almost_fill_a_blob());

    let requires_new_blob_but_doesnt_utilize_it =
        generate_block(1, enough_bytes_to_almost_fill_a_blob() / 3);

    let blocks: SequentialFuelBlocks = nonempty![
        stops_at_blob_boundary,
        requires_new_blob_but_doesnt_utilize_it
    ]
    .try_into()
    .unwrap();

    let mut bundler = Bundler::new(
        BlobEncoder,
        blocks.clone(),
        bundle::Encoder::new(CompressionLevel::Disabled),
        NonZeroUsize::new(1).unwrap(),
        1u16.into(),
    );

    bundler.advance(1.try_into().unwrap()).await?;

    let non_optimal_bundle = bundler.clone().finish().await?;
    bundler.advance(1.try_into().unwrap()).await?;

    // when
    let optimal_bundle = bundler.finish().await?;

    // then
    // Non-optimal bundle should include both blocks
    assert_eq!(non_optimal_bundle.metadata.block_heights, 0..=1);
    assert!(!non_optimal_bundle.metadata.known_to_be_optimal);

    // Optimal bundle should include only the first block
    assert_eq!(optimal_bundle.metadata.block_heights, 0..=0);
    assert!(optimal_bundle.metadata.known_to_be_optimal);

    Ok(())
}

#[tokio::test]
async fn bundler_tolerates_step_too_large() -> Result<()> {
    // given

    let blocks = generate_storage_block_sequence(0..=2, 300);

    let step_size = NonZeroUsize::new(5).unwrap(); // Step size larger than number of blocks

    let mut bundler = Bundler::new(
        BlobEncoder,
        blocks.clone(),
        bundle::Encoder::new(CompressionLevel::Disabled),
        step_size,
        1u16.into(),
    );

    while bundler.advance(1.try_into().unwrap()).await? {}

    // when
    let bundle = bundler.finish().await?;

    // then
    assert!(bundle.metadata.known_to_be_optimal);
    assert_eq!(bundle.metadata.block_heights, 0..=2);
    assert_eq!(bundle.metadata.optimization_attempts, 3); // 3 then 1 then 2

    Ok(())
}

// when the smaller bundle doesn't utilize the whole blob, for example
#[tokio::test]
async fn bigger_bundle_will_have_same_storage_gas_usage() -> Result<()> {
    // given
    let blocks = nonempty![
        generate_block(0, 100),
        generate_block(1, enough_bytes_to_almost_fill_a_blob())
    ];

    let mut bundler = Bundler::new(
        BlobEncoder,
        blocks.clone().try_into().unwrap(),
        bundle::Encoder::new(CompressionLevel::Disabled),
        NonZeroUsize::new(1).unwrap(), // Default step size
        1u16.into(),
    );
    while bundler.advance(1.try_into().unwrap()).await? {}

    // when
    let bundle = bundler.finish().await?;

    // then
    assert!(bundle.metadata.known_to_be_optimal);
    assert_eq!(bundle.metadata.block_heights, 0..=1);
    Ok(())
}

fn enough_bytes_to_almost_fill_a_blob() -> usize {
    let encoding_overhead = BlobEncoder::FRAGMENT_SIZE as f64 * 0.04;
    BlobEncoder::FRAGMENT_SIZE - encoding_overhead as usize
}

fn default_bundler_factory() -> BundlerFactory<BlobEncoder> {
    BundlerFactory::new(
        BlobEncoder,
        bundle::Encoder::new(CompressionLevel::Disabled),
        1.try_into().unwrap(),
    )
}

#[tokio::test]
async fn does_nothing_if_not_enough_blocks() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    setup
        .import_blocks(Blocks::WithHeights {
            range: 0..=0,
            data_size: 100,
        })
        .await;

    let num_blocks_to_accumulate = 2.try_into().unwrap();

    let mock_fuel_api = test_helpers::mocks::fuel::latest_height_is(0);

    let mut block_bundler = BlockBundler::new(
        mock_fuel_api,
        setup.db(),
        TestClock::default(),
        default_bundler_factory(),
        BlockBundlerConfig {
            num_blocks_to_accumulate,
            lookback_window: 0, // Adjust lookback_window as needed
            ..BlockBundlerConfig::default()
        },
    );

    // when
    block_bundler.run().await?;

    // then
    assert!(setup
        .db()
        .oldest_nonfinalized_fragments(0, 1)
        .await?
        .is_empty());

    Ok(())
}

#[tokio::test]
async fn stops_accumulating_blocks_if_time_runs_out_measured_from_component_creation() -> Result<()>
{
    // given
    let setup = test_helpers::Setup::init().await;

    let blocks = setup
        .import_blocks(Blocks::WithHeights {
            range: 0..=0,
            data_size: 100,
        })
        .await;

    let clock = TestClock::default();

    let latest_height = blocks.last().height;
    let mock_fuel_api = test_helpers::mocks::fuel::latest_height_is(latest_height);

    let expected_fragments = bundle_and_encode_into_blobs(blocks.clone(), 1);

    let mut block_bundler = BlockBundler::new(
        mock_fuel_api,
        setup.db(),
        clock.clone(),
        default_bundler_factory(),
        BlockBundlerConfig {
            block_accumulation_time_limit: Duration::from_secs(1),
            num_blocks_to_accumulate: 2.try_into().unwrap(),
            lookback_window: 0,
            ..Default::default()
        },
    );

    clock.advance_time(Duration::from_secs(2));

    // when
    block_bundler.run().await?;

    // then
    let fragments = setup
        .db()
        .oldest_nonfinalized_fragments(0, 1)
        .await?
        .into_iter()
        .map(|f| f.fragment)
        .collect_nonempty()
        .unwrap();

    assert_eq!(fragments, expected_fragments);

    assert!(setup
        .db()
        .lowest_sequence_of_unbundled_blocks(blocks.last().height, 1)
        .await?
        .is_none());

    Ok(())
}

#[tokio::test]
async fn stops_accumulating_blocks_if_time_runs_out_measured_from_last_bundle_time() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let clock = TestClock::default();

    let fuel_blocks = setup
        .import_blocks(Blocks::WithHeights {
            range: 1..=3,
            data_size: 100,
        })
        .await;

    let mut block_bundler = BlockBundler::new(
        mocks::fuel::latest_height_is(fuel_blocks.last().height),
        setup.db(),
        clock.clone(),
        default_bundler_factory(),
        BlockBundlerConfig {
            block_accumulation_time_limit: Duration::from_secs(10),
            num_blocks_to_accumulate: 2.try_into().unwrap(),
            ..Default::default()
        },
    );
    let fuel_blocks = Vec::from(fuel_blocks);

    block_bundler.run().await?;
    clock.advance_time(Duration::from_secs(10));

    // when
    block_bundler.run().await?;

    // then
    let first_bundle_fragments =
        bundle_and_encode_into_blobs(nonempty![fuel_blocks[0].clone(), fuel_blocks[1].clone()], 1);

    let second_bundle_fragments =
        bundle_and_encode_into_blobs(nonempty![fuel_blocks[2].clone()], 2);

    let unsubmitted_fragments = setup
        .db()
        .oldest_nonfinalized_fragments(0, 2)
        .await?
        .into_iter()
        .map(|f| f.fragment.clone())
        .collect_nonempty()
        .unwrap();

    let expected_fragments = first_bundle_fragments
        .into_iter()
        .chain(second_bundle_fragments)
        .collect_nonempty()
        .unwrap();
    assert_eq!(unsubmitted_fragments, expected_fragments);

    Ok(())
}

#[tokio::test]
async fn doesnt_bundle_more_than_accumulation_blocks() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let blocks = setup
        .import_blocks(Blocks::WithHeights {
            range: 0..=2,
            data_size: 100,
        })
        .await;

    let first_two_blocks = blocks.iter().take(2).cloned().collect_nonempty().unwrap();
    let fragments = bundle_and_encode_into_blobs(first_two_blocks, 1);

    let mut block_bundler = BlockBundler::new(
        test_helpers::mocks::fuel::latest_height_is(2),
        setup.db(),
        TestClock::default(),
        default_bundler_factory(),
        BlockBundlerConfig {
            num_blocks_to_accumulate: 2.try_into().unwrap(),
            ..Default::default()
        },
    );

    // when
    block_bundler.run().await?;

    // then
    let unsubmitted_fragments = setup
        .db()
        .oldest_nonfinalized_fragments(0, 10)
        .await?
        .into_iter()
        .map(|f| f.fragment)
        .collect_nonempty()
        .unwrap();

    assert_eq!(unsubmitted_fragments, fragments);

    Ok(())
}

#[tokio::test]
async fn doesnt_bundle_already_bundled_blocks() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let blocks = setup
        .import_blocks(Blocks::WithHeights {
            range: 0..=1,
            data_size: 100,
        })
        .await;

    let fragments_1 = bundle_and_encode_into_blobs(nonempty![blocks[0].clone()], 1);

    let fragments_2 = bundle_and_encode_into_blobs(nonempty![blocks[1].clone()], 2);

    let mut bundler = BlockBundler::new(
        test_helpers::mocks::fuel::latest_height_is(1),
        setup.db(),
        TestClock::default(),
        default_bundler_factory(),
        BlockBundlerConfig {
            num_blocks_to_accumulate: 1.try_into().unwrap(),
            ..Default::default()
        },
    );

    bundler.run().await?;

    // when
    bundler.run().await?;

    // then
    let unsubmitted_fragments = setup
        .db()
        .oldest_nonfinalized_fragments(0, usize::MAX)
        .await?;
    let db_fragments = unsubmitted_fragments
        .iter()
        .map(|f| f.fragment.clone())
        .collect::<Vec<_>>();
    let expected_fragments = fragments_1.into_iter().chain(fragments_2).collect_vec();
    assert_eq!(db_fragments, expected_fragments);

    Ok(())
}

#[tokio::test]
async fn stops_advancing_if_optimization_time_ran_out() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    setup
        .import_blocks(Blocks::WithHeights {
            range: 0..=0,
            data_size: 100,
        })
        .await;

    let unoptimal_fragments = nonempty![Fragment {
        data: test_helpers::random_data(100usize),
        unused_bytes: 1000,
        total_bytes: 50.try_into().unwrap(),
    }];

    let unoptimal_bundle = BundleProposal {
        fragments: unoptimal_fragments.clone(),
        metadata: Metadata {
            block_heights: 0..=0,
            known_to_be_optimal: false,
            gas_usage: 100,
            optimization_attempts: 10,
            compressed_data_size: 100.try_into().unwrap(),
            uncompressed_data_size: 1000.try_into().unwrap(),
            num_fragments: 1.try_into().unwrap(),
        },
    };

    let (bundler_factory, send_can_advance_permission, mut notify_has_advanced) =
        ControllableBundlerFactory::setup(Some(unoptimal_bundle));

    let test_clock = TestClock::default();

    let optimization_timeout = Duration::from_secs(1);

    let mut block_bundler = BlockBundler::new(
        test_helpers::mocks::fuel::latest_height_is(0),
        setup.db(),
        test_clock.clone(),
        bundler_factory,
        BlockBundlerConfig {
            optimization_time_limit: optimization_timeout,
            ..BlockBundlerConfig::default()
        },
    );

    let block_bundler_handle = tokio::spawn(async move {
        block_bundler.run().await.unwrap();
    });

    // when
    // Unblock the bundler
    send_can_advance_permission.send(()).unwrap();
    notify_has_advanced.recv().await.unwrap();

    // Advance the clock to exceed the optimization time limit
    test_clock.advance_time(Duration::from_secs(1));

    send_can_advance_permission.send(()).unwrap();

    // then
    // Wait for the BlockBundler task to complete
    block_bundler_handle.await.unwrap();

    Ok(())
}

#[tokio::test]
async fn doesnt_stop_advancing_if_there_is_still_time_to_optimize() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    setup
        .import_blocks(Blocks::WithHeights {
            range: 0..=0,
            data_size: 100,
        })
        .await;

    let (bundler_factory, send_can_advance, _notify_advanced) =
        ControllableBundlerFactory::setup(None);

    // Create a TestClock
    let test_clock = TestClock::default();

    // Create the BlockBundler
    let optimization_timeout = Duration::from_secs(1);

    let mut block_bundler = BlockBundler::new(
        test_helpers::mocks::fuel::latest_height_is(0),
        setup.db(),
        test_clock.clone(),
        bundler_factory,
        BlockBundlerConfig {
            optimization_time_limit: optimization_timeout,
            lookback_window: 0,
            ..BlockBundlerConfig::default()
        },
    );

    // Spawn the BlockBundler run method in a separate task
    let block_bundler_handle = tokio::spawn(async move {
        block_bundler.run().await.unwrap();
    });

    // Advance the clock but not beyond the optimization time limit
    test_clock.advance_time(Duration::from_millis(500));

    // when
    for _ in 0..100 {
        send_can_advance.send(()).unwrap();
    }
    // then
    let res = tokio::time::timeout(Duration::from_millis(500), block_bundler_handle).await;

    assert!(res.is_err(), "expected a timeout");

    Ok(())
}

#[tokio::test]
async fn skips_blocks_outside_lookback_window() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let blocks = setup
        .import_blocks(Blocks::WithHeights {
            range: 0..=3,
            data_size: 100,
        })
        .await;

    let lookback_window = 2;
    let latest_height = 5u32;

    let starting_height = latest_height.saturating_sub(lookback_window);

    let blocks_to_bundle: Vec<_> = blocks
        .iter()
        .filter(|block| block.height >= starting_height)
        .cloned()
        .collect();

    assert_eq!(
        blocks_to_bundle.len(),
        1,
        "Expected only one block to be within the lookback window"
    );
    assert_eq!(
        blocks_to_bundle[0].height, 3,
        "Expected block at height 3 to be within the lookback window"
    );

    // Encode the blocks to be bundled
    let expected_fragments =
        bundle_and_encode_into_blobs(NonEmpty::from_vec(blocks_to_bundle).unwrap(), 1);

    let mut block_bundler = BlockBundler::new(
        test_helpers::mocks::fuel::latest_height_is(latest_height),
        setup.db(),
        TestClock::default(),
        default_bundler_factory(),
        BlockBundlerConfig {
            num_blocks_to_accumulate: 1.try_into().unwrap(),
            lookback_window,
            ..Default::default()
        },
    );

    // when
    block_bundler.run().await?;

    // then
    let unsubmitted_fragments = setup
        .db()
        .oldest_nonfinalized_fragments(0, usize::MAX)
        .await?;
    let fragments = unsubmitted_fragments
        .iter()
        .map(|f| f.fragment.clone())
        .collect_nonempty()
        .unwrap();

    assert_eq!(
        fragments, expected_fragments,
        "Only blocks within the lookback window should be bundled"
    );

    // Ensure that blocks outside the lookback window are still unbundled
    let unbundled_blocks = setup
        .db()
        .lowest_sequence_of_unbundled_blocks(0, 10)
        .await?
        .unwrap();

    let unbundled_block_heights: Vec<_> = unbundled_blocks
        .into_inner()
        .iter()
        .map(|b| b.height)
        .collect();

    assert_eq!(
        unbundled_block_heights,
        vec![0, 1, 2],
        "Blocks outside the lookback window should remain unbundled"
    );

    Ok(())
}

#[tokio::test]
async fn metrics_are_updated() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    // Import two blocks with specific parameters
    let blocks = setup
        .import_blocks(Blocks::WithHeights {
            range: 0..=1,
            data_size: 100,
        })
        .await;

    let latest_height = blocks.last().height;
    let mock_fuel_api = test_helpers::mocks::fuel::latest_height_is(latest_height);

    let registry = metrics::prometheus::Registry::new();

    let mut block_bundler = BlockBundler::new(
        mock_fuel_api,
        setup.db(),
        TestClock::default(),
        default_bundler_factory(),
        BlockBundlerConfig {
            num_blocks_to_accumulate: NonZeroUsize::new(2).unwrap(),
            ..Default::default()
        },
    );

    block_bundler.register_metrics(&registry);

    // when
    block_bundler.run().await?;

    // then
    let gathered_metrics = registry.gather();

    // Check that the last_bundled_block_height metric has been updated correctly
    let last_bundled_block_height_metric = gathered_metrics
        .iter()
        .find(|metric| metric.get_name() == "last_bundled_block_height")
        .expect("last_bundled_block_height metric not found");

    let last_bundled_block_height = last_bundled_block_height_metric
        .get_metric()
        .first()
        .expect("No metric samples found")
        .get_gauge()
        .get_value() as i64;

    assert_eq!(last_bundled_block_height, blocks.last().height as i64);

    // Check that the blocks_per_bundle metric has recorded the correct number of blocks
    let blocks_per_bundle_metric = gathered_metrics
        .iter()
        .find(|metric| metric.get_name() == "blocks_per_bundle")
        .expect("blocks_per_bundle metric not found");

    let blocks_per_bundle_sample = blocks_per_bundle_metric
        .get_metric()
        .first()
        .expect("No metric samples found")
        .get_histogram();

    // The sample count should be 1 (since we observed once)
    let blocks_per_bundle_count = blocks_per_bundle_sample.get_sample_count();
    assert_eq!(blocks_per_bundle_count, 1);

    // The sample sum should be 2.0 (since we bundled 2 blocks)
    let blocks_per_bundle_sum = blocks_per_bundle_sample.get_sample_sum();
    assert_eq!(blocks_per_bundle_sum, 2.0);

    let compression_ratio_metric = gathered_metrics
        .iter()
        .find(|metric| metric.get_name() == "compression_ratio")
        .expect("compression_ratio metric not found");

    let compression_ratio_sample = compression_ratio_metric
        .get_metric()
        .first()
        .expect("No metric samples found")
        .get_histogram();

    let compression_ratio_count = compression_ratio_sample.get_sample_count();
    assert_eq!(compression_ratio_count, 1);

    let compression_ratio_sum = compression_ratio_sample.get_sample_sum();
    // If we don't compress we loose a bit due to postcard encoding the bundle
    assert!((0.97..=1.0).contains(&compression_ratio_sum));

    Ok(())
}
