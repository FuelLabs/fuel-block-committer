use std::{num::NonZeroUsize, time::Duration};

pub mod bundler;

use bundler::{Bundle, BundleProposal, BundlerFactory, Metadata};
use metrics::{
    custom_exponential_buckets,
    prometheus::{histogram_opts, linear_buckets, Histogram, IntGauge},
    RegistersMetrics,
};
use ports::{
    clock::Clock,
    storage::Storage,
    types::{DateTime, Utc},
};
use tracing::info;

use crate::{Error, Result, Runner};

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub optimization_time_limit: Duration,
    pub max_bundles_per_optimization_run: NonZeroUsize,
    pub block_accumulation_time_limit: Duration,
    pub num_blocks_to_accumulate: NonZeroUsize,
    pub lookback_window: u32,
}

#[cfg(test)]
impl Default for Config {
    fn default() -> Self {
        Self {
            optimization_time_limit: Duration::from_secs(100),
            block_accumulation_time_limit: Duration::from_secs(100),
            num_blocks_to_accumulate: NonZeroUsize::new(1).unwrap(),
            lookback_window: 1000,
            max_bundles_per_optimization_run: 1.try_into().unwrap(),
        }
    }
}

/// The `BlockBundler` bundles blocks and fragments them. Those fragments are later on submitted to
/// l1 by the [`crate::StateCommitter`]
pub struct BlockBundler<F, Storage, Clock, BundlerFactory> {
    fuel_api: F,
    storage: Storage,
    clock: Clock,
    bundler_factory: BundlerFactory,
    config: Config,
    last_time_bundled: DateTime<Utc>,
    metrics: Metrics,
}

impl<F, S, C, B> RegistersMetrics for BlockBundler<F, S, C, B> {
    fn metrics(&self) -> Vec<Box<dyn metrics::prometheus::core::Collector>> {
        vec![
            Box::new(self.metrics.blocks_per_bundle.clone()),
            Box::new(self.metrics.last_bundled_block_height.clone()),
            Box::new(self.metrics.compression_ratio.clone()),
            Box::new(self.metrics.optimization_duration.clone()),
        ]
    }
}

#[derive(Debug, Clone)]
struct Metrics {
    blocks_per_bundle: Histogram,
    last_bundled_block_height: IntGauge,
    compression_ratio: Histogram,
    optimization_duration: Histogram,
}

impl Metrics {
    fn observe_metadata(&self, metadata: &Metadata) {
        self.blocks_per_bundle.observe(metadata.num_blocks() as f64);
        self.compression_ratio.observe(metadata.compression_ratio());
        self.last_bundled_block_height
            .set((*metadata.block_heights.end()).into());
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let blocks_per_bundle = Histogram::with_opts(histogram_opts!(
            "blocks_per_bundle",
            "Number of blocks per bundle",
            linear_buckets(250.0, 250.0, 3600 / 250).expect("to be correctly configured")
        ))
        .expect("to be correctly configured");

        let last_bundled_block_height = IntGauge::new(
            "last_bundled_block_height",
            "The height of the last bundled block",
        )
        .expect("to be correctly configured");

        let compression_ratio = Histogram::with_opts(histogram_opts!(
            "compression_ratio",
            "The compression ratio of the bundled data",
            custom_exponential_buckets(1.0, 10.0, 7)
        ))
        .expect("to be correctly configured");

        let optimization_duration = Histogram::with_opts(histogram_opts!(
            "optimization_duration",
            "The duration of the optimization phase in seconds",
            custom_exponential_buckets(60.0, 300.0, 10)
        ))
        .expect("to be correctly configured");

        Self {
            blocks_per_bundle,
            last_bundled_block_height,
            compression_ratio,
            optimization_duration,
        }
    }
}

impl<F, Storage, C, BF> BlockBundler<F, Storage, C, BF>
where
    C: Clock,
{
    /// Creates a new `BlockBundler`.
    pub fn new(
        fuel_adapter: F,
        storage: Storage,
        clock: C,
        bundler_factory: BF,
        config: Config,
    ) -> Self {
        let now = clock.now();

        Self {
            fuel_api: fuel_adapter,
            storage,
            clock,
            last_time_bundled: now,
            bundler_factory,
            config,
            metrics: Metrics::default(),
        }
    }
}

impl<F, Db, C, BF> BlockBundler<F, Db, C, BF>
where
    F: ports::fuel::Api,
    Db: Storage,
    C: Clock,
    BF: BundlerFactory,
{
    async fn bundle_and_fragment_blocks(&mut self) -> Result<()> {
        let starting_height = self.get_starting_height().await?;

        while let Some(blocks) = self
            .storage
            .lowest_sequence_of_unbundled_blocks(
                starting_height,
                self.config.num_blocks_to_accumulate.get(),
            )
            .await?
        {
            let still_time_to_accumulate_more = self.still_time_to_accumulate_more().await?;
            if blocks.len() < self.config.num_blocks_to_accumulate && still_time_to_accumulate_more
            {
                info!(
                    "Not enough blocks ({} < {}) to bundle. Waiting for more to accumulate.",
                    blocks.len(),
                    self.config.num_blocks_to_accumulate.get()
                );

                return Ok(());
            }

            if !still_time_to_accumulate_more {
                info!("Accumulation time limit reached.",);
            }

            info!("Giving {} blocks to the bundler", blocks.len());

            let next_id = self.storage.next_bundle_id().await?;
            let bundler = self.bundler_factory.build(blocks, next_id).await;

            let optimization_start = self.clock.now();
            let BundleProposal {
                fragments,
                metadata,
            } = self.find_optimal_bundle(bundler).await?;

            let optimization_duration = self.clock.now().signed_duration_since(optimization_start);

            info!("Bundler proposed: {metadata}");

            self.storage
                .insert_bundle_and_fragments(next_id, metadata.block_heights.clone(), fragments)
                .await?;

            self.metrics.observe_metadata(&metadata);
            self.metrics
                .optimization_duration
                .observe(optimization_duration.num_seconds() as f64);

            self.last_time_bundled = self.clock.now();
        }

        Ok(())
    }

    async fn get_starting_height(&self) -> Result<u32> {
        let current_height = self.fuel_api.latest_height().await?;
        let starting_height = current_height.saturating_sub(self.config.lookback_window);

        Ok(starting_height)
    }

    async fn find_optimal_bundle<B: Bundle>(&self, mut bundler: B) -> Result<BundleProposal> {
        let optimization_start = self.clock.now();

        while bundler
            .advance(self.config.max_bundles_per_optimization_run)
            .await?
        {
            if self.should_stop_optimizing(optimization_start)? {
                info!("Optimization time limit reached! Finishing bundling.");
                break;
            }
        }

        bundler.finish().await
    }

    async fn still_time_to_accumulate_more(&self) -> Result<bool> {
        let elapsed = self.elapsed(self.last_time_bundled)?;

        Ok(elapsed < self.config.block_accumulation_time_limit)
    }

    fn elapsed(&self, point: DateTime<Utc>) -> Result<Duration> {
        let now = self.clock.now();
        let elapsed = now
            .signed_duration_since(point)
            .to_std()
            .map_err(|e| Error::Other(format!("could not calculate elapsed time: {e}")))?;
        Ok(elapsed)
    }

    fn should_stop_optimizing(&self, start_of_optimization: DateTime<Utc>) -> Result<bool> {
        let elapsed = self.elapsed(start_of_optimization)?;

        Ok(elapsed >= self.config.optimization_time_limit)
    }
}

impl<F, Db, C, BF> Runner for BlockBundler<F, Db, C, BF>
where
    F: ports::fuel::Api + Send + Sync,
    Db: Storage + Clone + Send + Sync,
    C: Clock + Send + Sync,
    BF: BundlerFactory + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        self.bundle_and_fragment_blocks().await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bundler::Metadata;
    use clock::TestClock;
    use eth::BlobEncoder;
    use itertools::Itertools;
    use ports::{
        l1::FragmentEncoder,
        storage::SequentialFuelBlocks,
        types::{nonempty, CollectNonEmpty, CompressedFuelBlock, Fragment, NonEmpty, NonNegative},
    };
    use tokio::sync::{
        mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
        Mutex,
    };
    use utils::bundle::{self, CompressionLevel};

    use super::*;
    use crate::test_utils::{self, mocks, Blocks};

    struct ControllableBundler {
        can_advance: UnboundedReceiver<()>,
        notify_advanced: UnboundedSender<()>,
        proposal: Option<BundleProposal>,
    }

    impl ControllableBundler {
        pub fn create(
            proposal: Option<BundleProposal>,
        ) -> (Self, UnboundedSender<()>, UnboundedReceiver<()>) {
            let (send_can_advance, recv_can_advance) = unbounded_channel::<()>();
            let (notify_advanced, recv_advanced_notif) = unbounded_channel::<()>();
            (
                Self {
                    can_advance: recv_can_advance,
                    notify_advanced,
                    proposal,
                },
                send_can_advance,
                recv_advanced_notif,
            )
        }
    }

    impl Bundle for ControllableBundler {
        async fn advance(&mut self, _: NonZeroUsize) -> Result<bool> {
            self.can_advance.recv().await.unwrap();
            self.notify_advanced.send(()).unwrap();
            Ok(true)
        }

        async fn finish(self) -> Result<BundleProposal> {
            Ok(self.proposal.expect(
                "proposal to be set inside controllable bundler if it ever was meant to finish",
            ))
        }
    }

    struct ControllableBundlerFactory {
        bundler: Mutex<Option<ControllableBundler>>,
    }

    impl ControllableBundlerFactory {
        pub fn setup(
            proposal: Option<BundleProposal>,
        ) -> (Self, UnboundedSender<()>, UnboundedReceiver<()>) {
            let (bundler, send_can_advance, receive_advanced) =
                ControllableBundler::create(proposal);
            (
                Self {
                    bundler: Mutex::new(Some(bundler)),
                },
                send_can_advance,
                receive_advanced,
            )
        }
    }

    impl BundlerFactory for ControllableBundlerFactory {
        type Bundler = ControllableBundler;

        async fn build(&self, _: SequentialFuelBlocks, _: NonNegative<i32>) -> Self::Bundler {
            self.bundler.lock().await.take().unwrap()
        }
    }

    fn default_bundler_factory() -> bundler::Factory<BlobEncoder> {
        bundler::Factory::new(
            BlobEncoder,
            bundle::Encoder::new(CompressionLevel::Disabled),
            1.try_into().unwrap(),
        )
    }

    #[tokio::test]
    async fn does_nothing_if_not_enough_blocks() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;
        setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=0,
                data_size: 100,
            })
            .await;

        let num_blocks_to_accumulate = 2.try_into().unwrap();

        let mock_fuel_api = test_utils::mocks::fuel::latest_height_is(0);

        let mut block_bundler = BlockBundler::new(
            mock_fuel_api,
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config {
                num_blocks_to_accumulate,
                lookback_window: 0, // Adjust lookback_window as needed
                ..Config::default()
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

    fn merge_data(blocks: impl IntoIterator<Item = CompressedFuelBlock>) -> NonEmpty<u8> {
        blocks
            .into_iter()
            .flat_map(|b| b.data)
            .collect_nonempty()
            .expect("is not empty")
    }

    #[tokio::test]
    async fn stops_accumulating_blocks_if_time_runs_out_measured_from_component_creation(
    ) -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let blocks = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=0,
                data_size: 100,
            })
            .await;

        let clock = TestClock::default();

        let latest_height = blocks.last().height;
        let mock_fuel_api = test_utils::mocks::fuel::latest_height_is(latest_height);

        let data = merge_data(blocks.clone());
        let expected_fragments = BlobEncoder.encode(data, 1.into()).unwrap();

        let mut block_bundler = BlockBundler::new(
            mock_fuel_api,
            setup.db(),
            clock.clone(),
            default_bundler_factory(),
            Config {
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
    async fn stops_accumulating_blocks_if_time_runs_out_measured_from_last_bundle_time(
    ) -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

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
            Config {
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
        let first_bundle = merge_data(fuel_blocks[0..=1].to_vec());
        let first_bundle_fragments = BlobEncoder.encode(first_bundle, 1.into()).unwrap();

        let second_bundle = merge_data(fuel_blocks[2..=2].to_vec());
        let second_bundle_fragments = BlobEncoder.encode(second_bundle, 2.into()).unwrap();

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
        let setup = test_utils::Setup::init().await;

        let blocks = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=2,
                data_size: 100,
            })
            .await;

        let first_two_blocks = blocks.iter().take(2).cloned().collect_nonempty().unwrap();
        let bundle_data = merge_data(first_two_blocks);
        let fragments = BlobEncoder.encode(bundle_data, 1.into()).unwrap();

        let mut block_bundler = BlockBundler::new(
            test_utils::mocks::fuel::latest_height_is(2),
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config {
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
        let setup = test_utils::Setup::init().await;

        let blocks = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=1,
                data_size: 100,
            })
            .await;

        let fragments_1 = BlobEncoder
            .encode(blocks.first().data.clone(), 1.into())
            .unwrap();

        let fragments_2 = BlobEncoder
            .encode(blocks.last().data.clone(), 2.into())
            .unwrap();

        let mut bundler = BlockBundler::new(
            test_utils::mocks::fuel::latest_height_is(1),
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config {
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
        let fragments = unsubmitted_fragments
            .iter()
            .map(|f| f.fragment.clone())
            .collect::<Vec<_>>();
        let all_fragments = fragments_1.into_iter().chain(fragments_2).collect_vec();
        assert_eq!(fragments, all_fragments);

        Ok(())
    }

    #[tokio::test]
    async fn stops_advancing_if_optimization_time_ran_out() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;
        setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=0,
                data_size: 100,
            })
            .await;

        let unoptimal_fragments = nonempty![Fragment {
            data: test_utils::random_data(100usize),
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
            test_utils::mocks::fuel::latest_height_is(0),
            setup.db(),
            test_clock.clone(),
            bundler_factory,
            Config {
                optimization_time_limit: optimization_timeout,
                ..Config::default()
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
        let setup = test_utils::Setup::init().await;
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
            test_utils::mocks::fuel::latest_height_is(0),
            setup.db(),
            test_clock.clone(),
            bundler_factory,
            Config {
                optimization_time_limit: optimization_timeout,
                lookback_window: 0,
                ..Config::default()
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
        let setup = test_utils::Setup::init().await;

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
        let data = merge_data(blocks_to_bundle.clone());
        let expected_fragments = BlobEncoder.encode(data, 1.into()).unwrap();

        let mut block_bundler = BlockBundler::new(
            test_utils::mocks::fuel::latest_height_is(latest_height),
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config {
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
        let setup = test_utils::Setup::init().await;

        // Import two blocks with specific parameters
        let blocks = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=1,
                data_size: 100,
            })
            .await;

        let latest_height = blocks.last().height;
        let mock_fuel_api = test_utils::mocks::fuel::latest_height_is(latest_height);

        let registry = metrics::prometheus::Registry::new();

        let mut block_bundler = BlockBundler::new(
            mock_fuel_api,
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config {
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
        assert_eq!(compression_ratio_sum, 1.0); // Compression ratio is 1.0 when compression is disabled

        Ok(())
    }
}
