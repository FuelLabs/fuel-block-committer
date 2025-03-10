pub mod bundler;
pub mod common;
pub mod eigen_bundler; // TODO

pub mod service {
    use std::{num::NonZeroUsize, time::Duration};

    use byte_unit::Byte;
    use metrics::{
        RegistersMetrics, custom_exponential_buckets,
        prometheus::{Histogram, IntGauge, histogram_opts, linear_buckets},
    };
    use tracing::info;

    use super::{
        common::{Bundle, BundleProposal, BundlerFactory, Metadata},
        port::UnbundledBlocks,
    };
    use crate::{
        Error, Result, Runner,
        types::{DateTime, Utc, storage::SequentialFuelBlocks},
    };

    #[derive(Debug, Clone, Copy)]
    pub struct Config {
        pub optimization_time_limit: Duration,
        pub max_bundles_per_optimization_run: NonZeroUsize,
        pub max_fragments_per_bundle: NonZeroUsize,
        pub accumulation_time_limit: Duration,
        pub bytes_to_accumulate: NonZeroUsize,
        pub blocks_to_accumulate: NonZeroUsize,
        pub lookback_window: u32,
    }

    #[cfg(feature = "test-helpers")]
    impl Default for Config {
        fn default() -> Self {
            Self {
                optimization_time_limit: Duration::from_secs(100),
                accumulation_time_limit: Duration::from_secs(100),
                bytes_to_accumulate: NonZeroUsize::new(1).unwrap(),
                max_fragments_per_bundle: NonZeroUsize::MAX,
                lookback_window: 1000,
                max_bundles_per_optimization_run: 1.try_into().unwrap(),
                blocks_to_accumulate: NonZeroUsize::new(10).unwrap(),
            }
        }
    }

    /// The `BlockBundler` bundles blocks and fragments them. Those fragments are later on submitted to
    /// l1 by the [`crate::StateCommitter`]
    pub struct BlockBundler<FuelApi, Storage, Clock, BundlerFactory> {
        fuel_api: FuelApi,
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

    impl<FuelApi, Storage, Clock, BF> BlockBundler<FuelApi, Storage, Clock, BF>
    where
        Clock: crate::block_bundler::port::Clock,
    {
        /// Creates a new `BlockBundler`.
        pub fn new(
            fuel_adapter: FuelApi,
            storage: Storage,
            clock: Clock,
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

    impl<FuelApi, Db, Clock, BF> BlockBundler<FuelApi, Db, Clock, BF>
    where
        FuelApi: crate::block_bundler::port::fuel::Api,
        Db: crate::block_bundler::port::Storage,
        Clock: crate::block_bundler::port::Clock,
        BF: BundlerFactory,
    {
        async fn bundle_and_fragment_blocks(&mut self) -> Result<()> {
            let starting_height = self.get_starting_height().await?;

            while let Some(UnbundledBlocks {
                oldest,
                total_unbundled,
            }) = self
                .storage
                .lowest_sequence_of_unbundled_blocks(
                    starting_height,
                    self.config.bytes_to_accumulate.get() as u32,
                )
                .await?
            {
                if self.should_wait(&oldest, total_unbundled)? {
                    return Ok(());
                }

                let next_id = self.storage.next_bundle_id().await?;
                let bundler = self.bundler_factory.build(oldest, next_id).await;

                let optimization_start = self.clock.now();
                let BundleProposal {
                    fragments,
                    metadata,
                } = self.find_optimal_bundle(bundler).await?;

                let optimization_duration =
                    self.clock.now().signed_duration_since(optimization_start);

                tracing::info!("Bundler proposed: {metadata}");

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

        fn should_wait(
            &self,
            blocks: &SequentialFuelBlocks,
            total_available: NonZeroUsize,
        ) -> Result<bool> {
            let cum_size = blocks.cumulative_size();
            let has_more = total_available > blocks.len();

            let still_time_to_accumulate_more = self.still_time_to_accumulate_more()?;
            // We use `total_available` because we previously encountered a scenario with:
            // - A few very large blocks,
            // - Followed by a long sequence of bundled blocks,
            // - And then additional unbundled blocks.
            // Since bundling required a fixed number of sequential blocks, the process skipped over
            // the blocks before the gap until a timeout occurred. Even after timing out, only one bundle
            // was created, and then the system waited for another timeout. To avoid this, we ignore the
            // total count of unbundled blocks when deciding whether to wait. The trade-off is that if there
            // is a gap of unimported blocks followed by many unbundled blocks, processing of the newer
            // blocks is deferred until the older ones are bundled. This can lead to the creation of small
            // bundles if the import process cannot supply blocks quickly enough.

            let should_wait = cum_size < self.config.bytes_to_accumulate
                && total_available < self.config.blocks_to_accumulate
                && !has_more
                && still_time_to_accumulate_more;

            let available_data = human_readable_size(cum_size);

            if should_wait {
                let needed_data = human_readable_size(self.config.bytes_to_accumulate);

                let until_timeout = humantime::format_duration(
                    self.config
                        .accumulation_time_limit
                        .checked_sub(self.elapsed(self.last_time_bundled)?)
                        .unwrap_or_default(),
                );

                tracing::info!(
                    "Not bundling yet (accumulated {available_data} of required {needed_data}, {total_available}/{} blocks accumulated, timeout in {until_timeout}); waiting for more.",
                    self.config.blocks_to_accumulate
                );
            } else {
                tracing::info!(
                    "Proceeding to bundle with {} blocks (accumulated {available_data}).",
                    blocks.len()
                );
            }

            Ok(should_wait)
        }

        async fn get_starting_height(&self) -> Result<u32> {
            let current_height = self.fuel_api.latest_height().await?;
            let starting_height = current_height.saturating_sub(self.config.lookback_window);

            Ok(starting_height)
        }

        async fn find_optimal_bundle<B: Bundle>(&self, mut bundler: B) -> Result<BundleProposal> {
            // TODO: The current approach can lead to excessive optimization time when we are far behind. Maybe we should scale the optimization time depending on how behind bundling we are.
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

        fn still_time_to_accumulate_more(&self) -> Result<bool> {
            let elapsed = self.elapsed(self.last_time_bundled)?;

            Ok(elapsed < self.config.accumulation_time_limit)
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

    fn human_readable_size(num_bytes: std::num::NonZero<usize>) -> String {
        let unit = Byte::from_u64(num_bytes.get() as u64)
            .get_appropriate_unit(byte_unit::UnitType::Decimal);
        format!("{unit:.3}")
    }

    impl<FuelApi, Db, Clock, BF> Runner for BlockBundler<FuelApi, Db, Clock, BF>
    where
        FuelApi: crate::block_bundler::port::fuel::Api + Send + Sync,
        Db: crate::block_bundler::port::Storage + Clone + Send + Sync,
        Clock: crate::block_bundler::port::Clock + Send + Sync,
        BF: BundlerFactory + Send + Sync,
    {
        async fn run(&mut self) -> Result<()> {
            self.bundle_and_fragment_blocks().await?;

            Ok(())
        }
    }
}

pub mod port {
    use std::{num::NonZeroUsize, ops::RangeInclusive};

    use nonempty::NonEmpty;

    use crate::{
        Result,
        types::{DateTime, Fragment, NonNegative, Utc, storage::SequentialFuelBlocks},
    };

    pub mod fuel {
        #[allow(async_fn_in_trait)]
        #[trait_variant::make(Send)]
        #[cfg_attr(feature = "test-helpers", mockall::automock)]
        pub trait Api: Sync {
            async fn latest_height(&self) -> crate::Result<u32>;
        }
    }

    pub mod l1 {
        use std::num::NonZeroUsize;

        use nonempty::NonEmpty;

        use crate::{
            Result,
            types::{Fragment, NonNegative},
        };

        pub trait FragmentEncoder {
            fn encode(
                &self,
                data: NonEmpty<u8>,
                id: NonNegative<i32>,
            ) -> Result<NonEmpty<Fragment>>;
            fn gas_usage(&self, num_bytes: NonZeroUsize) -> u64;
            fn num_fragments_needed(&self, num_bytes: NonZeroUsize) -> NonZeroUsize;
        }
    }

    #[derive(Debug, Clone)]
    pub struct UnbundledBlocks {
        pub oldest: SequentialFuelBlocks,
        pub total_unbundled: NonZeroUsize,
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Sync {
        async fn lowest_sequence_of_unbundled_blocks(
            &self,
            starting_height: u32,
            max_cumulative_bytes: u32,
        ) -> Result<Option<UnbundledBlocks>>;
        async fn insert_bundle_and_fragments(
            &self,
            bundle_id: NonNegative<i32>,
            block_range: RangeInclusive<u32>,
            fragments: NonEmpty<Fragment>,
        ) -> Result<()>;
        async fn next_bundle_id(&self) -> Result<NonNegative<i32>>;
    }

    pub trait Clock {
        fn now(&self) -> DateTime<Utc>;
    }
}

#[cfg(feature = "test-helpers")]
pub mod test_helpers {
    use std::num::NonZeroUsize;

    use tokio::sync::{
        Mutex,
        mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    };

    use super::common::{Bundle, BundleProposal, BundlerFactory};
    use crate::types::{storage::SequentialFuelBlocks, NonNegative};

    pub struct ControllableBundler {
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
        async fn advance(&mut self, _: NonZeroUsize) -> crate::Result<bool> {
            self.can_advance.recv().await.unwrap();
            self.notify_advanced.send(()).unwrap();
            Ok(true)
        }

        async fn finish(self) -> crate::Result<BundleProposal> {
            Ok(self.proposal.expect(
                "proposal to be set inside controllable bundler if it ever was meant to finish",
            ))
        }
    }

    pub struct ControllableBundlerFactory {
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
}
