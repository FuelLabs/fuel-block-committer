pub mod bundler;

pub mod service {
    use std::{num::NonZeroUsize, time::Duration};

    use metrics::{
        custom_exponential_buckets,
        prometheus::{histogram_opts, linear_buckets, Histogram, IntGauge},
        RegistersMetrics,
    };
    use tracing::info;

    use super::bundler::{Bundle, BundleProposal, BundlerFactory, Metadata};
    use crate::{
        types::{DateTime, Utc},
        Error, Result, Runner,
    };

    #[derive(Debug, Clone, Copy)]
    pub struct Config {
        pub optimization_time_limit: Duration,
        pub max_bundles_per_optimization_run: NonZeroUsize,
        pub block_accumulation_time_limit: Duration,
        pub num_blocks_to_accumulate: NonZeroUsize,
        pub lookback_window: u32,
    }

    #[cfg(feature = "test-helpers")]
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

            while let Some(blocks) = self
                .storage
                .lowest_sequence_of_unbundled_blocks(
                    starting_height,
                    self.config.num_blocks_to_accumulate.get(),
                )
                .await?
            {
                let still_time_to_accumulate_more = self.still_time_to_accumulate_more()?;
                if blocks.len() < self.config.num_blocks_to_accumulate
                    && still_time_to_accumulate_more
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

                let optimization_duration =
                    self.clock.now().signed_duration_since(optimization_start);

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

        fn still_time_to_accumulate_more(&self) -> Result<bool> {
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
    use std::ops::RangeInclusive;

    use nonempty::NonEmpty;

    use crate::{
        types::{storage::SequentialFuelBlocks, DateTime, Fragment, NonNegative, Utc},
        Result,
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
            types::{Fragment, NonNegative},
            Result,
        };

        pub trait FragmentEncoder {
            fn encode(
                &self,
                data: NonEmpty<u8>,
                id: NonNegative<i32>,
            ) -> Result<NonEmpty<Fragment>>;
            fn gas_usage(&self, num_bytes: NonZeroUsize) -> u64;
        }
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Sync {
        async fn lowest_sequence_of_unbundled_blocks(
            &self,
            starting_height: u32,
            limit: usize,
        ) -> Result<Option<SequentialFuelBlocks>>;
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
        mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
        Mutex,
    };

    use super::bundler::{Bundle, BundleProposal, BundlerFactory};
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
