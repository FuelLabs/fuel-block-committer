use std::{num::NonZeroUsize, time::Duration};

pub mod bundler;

use bundler::{Bundle, BundleProposal, BundlerFactory};
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
        }
    }
}

/// The `BlockBundler` bundles blocks and fragments them. Those fragments are later on submitted to
/// l1 by the [`crate::StateCommitter`]
pub struct BlockBundler<F, Storage, Clock, BundlerFactory> {
    fuel_api: F,
    storage: Storage,
    clock: Clock,
    component_created_at: DateTime<Utc>,
    bundler_factory: BundlerFactory,
    config: Config,
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
            component_created_at: now,
            bundler_factory,
            config,
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
    async fn bundle_and_fragment_blocks(&self) -> Result<()> {
        let starting_height = self.get_starting_height().await?;
        let Some(blocks) = self
            .storage
            .lowest_sequence_of_unbundled_blocks(
                starting_height,
                self.config.num_blocks_to_accumulate.get(),
            )
            .await?
        else {
            return Ok(());
        };

        let still_time_to_accumulate_more = self.still_time_to_accumulate_more().await?;
        if blocks.len() < self.config.num_blocks_to_accumulate && still_time_to_accumulate_more {
            info!(
                "Not enough blocks ({} < {}) to bundle. Waiting for more to accumulate.",
                blocks.len(),
                self.config.num_blocks_to_accumulate.get()
            );

            return Ok(());
        }

        if !still_time_to_accumulate_more {
            info!(
                "Accumulation time limit reached. Giving {} blocks to the bundler.",
                blocks.len()
            );
        }

        let bundler = self.bundler_factory.build(blocks).await;

        let BundleProposal {
            fragments,
            metadata,
        } = self.find_optimal_bundle(bundler).await?;

        info!("Bundler proposed: {metadata}");

        self.storage
            .insert_bundle_and_fragments(metadata.block_heights, fragments)
            .await?;

        Ok(())
    }

    async fn get_starting_height(&self) -> Result<u32> {
        let current_height = self.fuel_api.latest_height().await?;
        let starting_height = current_height.saturating_sub(self.config.lookback_window);
        Ok(starting_height)
    }

    async fn find_optimal_bundle<B: Bundle>(&self, mut bundler: B) -> Result<BundleProposal> {
        let optimization_start = self.clock.now();

        while bundler.advance(32.try_into().expect("not zero")).await? {
            if self.should_stop_optimizing(optimization_start)? {
                info!("Optimization time limit reached! Finishing bundling.");
                break;
            }
        }

        bundler.finish().await
    }

    async fn still_time_to_accumulate_more(&self) -> Result<bool> {
        let last_finalized_time = self
            .storage
            .last_time_a_fragment_was_finalized()
            .await?
            .unwrap_or_else(||{
                info!("No finalized fragments found in storage. Using component creation time ({}) as last finalized time.", self.component_created_at);
                self.component_created_at
            });

        let elapsed = self.elapsed(last_finalized_time)?;

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
    use eth::Eip4844BlobEncoder;
    use itertools::Itertools;
    use ports::{
        l1::FragmentEncoder,
        storage::SequentialFuelBlocks,
        types::{nonempty, CollectNonEmpty, Fragment, NonEmpty},
    };
    use tokio::sync::{
        mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
        Mutex,
    };

    use super::*;
    use crate::{
        test_utils::{self, encode_and_merge, Blocks, ImportedBlocks},
        CompressionLevel,
    };

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

        async fn build(&self, _: SequentialFuelBlocks) -> Self::Bundler {
            self.bundler.lock().await.take().unwrap()
        }
    }

    fn default_bundler_factory() -> bundler::Factory<Eip4844BlobEncoder> {
        bundler::Factory::new(
            Eip4844BlobEncoder,
            CompressionLevel::Disabled,
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
                tx_per_block: 1,
                size_per_tx: 100,
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

    #[tokio::test]
    async fn stops_accumulating_blocks_if_time_runs_out_measured_from_component_creation(
    ) -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let ImportedBlocks {
            fuel_blocks: blocks,
            ..
        } = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=0,
                tx_per_block: 1,
                size_per_tx: 100,
            })
            .await;
        let data = encode_and_merge(blocks.clone());
        let expected_fragments = Eip4844BlobEncoder.encode(data).unwrap();

        let clock = TestClock::default();

        let latest_height = blocks.last().header.height;
        let mock_fuel_api = test_utils::mocks::fuel::latest_height_is(latest_height);

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
            .lowest_sequence_of_unbundled_blocks(blocks.last().header.height, 1)
            .await?
            .is_none());

        Ok(())
    }

    #[tokio::test]
    async fn stops_accumulating_blocks_if_time_runs_out_measured_from_last_finalized() -> Result<()>
    {
        // given
        let setup = test_utils::Setup::init().await;

        let clock = TestClock::default();
        setup.commit_single_block_bundle(clock.now()).await;
        clock.advance_time(Duration::from_secs(10));

        let ImportedBlocks { fuel_blocks, .. } = setup
            .import_blocks(Blocks::WithHeights {
                range: 1..=1,
                tx_per_block: 1,
                size_per_tx: 100,
            })
            .await;
        let data = encode_and_merge(fuel_blocks.clone());
        let expected_fragments = Eip4844BlobEncoder.encode(data).unwrap();

        let latest_height = fuel_blocks.last().header.height;
        let mock_fuel_api = test_utils::mocks::fuel::latest_height_is(latest_height);

        let mut block_bundler = BlockBundler::new(
            mock_fuel_api,
            setup.db(),
            clock.clone(),
            default_bundler_factory(),
            Config {
                block_accumulation_time_limit: Duration::from_secs(10),
                num_blocks_to_accumulate: 2.try_into().unwrap(),
                ..Default::default()
            },
        );

        // when
        block_bundler.run().await?;

        // then
        // We will bundle and fragment because the time limit (10s) is measured from the last finalized fragment

        let unsubmitted_fragments = setup
            .db()
            .oldest_nonfinalized_fragments(0, 1)
            .await?
            .into_iter()
            .map(|f| f.fragment.clone())
            .collect_nonempty()
            .unwrap();

        assert_eq!(unsubmitted_fragments, expected_fragments);

        Ok(())
    }

    #[tokio::test]
    async fn doesnt_bundle_more_than_accumulation_blocks() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let ImportedBlocks {
            fuel_blocks: blocks,
            ..
        } = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=2,
                tx_per_block: 1,
                size_per_tx: 100,
            })
            .await;

        let first_two_blocks = blocks.iter().take(2).cloned().collect_nonempty().unwrap();
        let bundle_data = test_utils::encode_and_merge(first_two_blocks.clone());
        let fragments = Eip4844BlobEncoder.encode(bundle_data).unwrap();

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

        let ImportedBlocks {
            fuel_blocks: blocks,
            ..
        } = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=1,
                tx_per_block: 1,
                size_per_tx: 100,
            })
            .await;

        let block_1 = nonempty![blocks.first().clone()];
        let bundle_1 = test_utils::encode_and_merge(block_1.clone());
        let fragments_1 = Eip4844BlobEncoder.encode(bundle_1).unwrap();

        let block_2 = nonempty![blocks.last().clone()];
        let bundle_2 = test_utils::encode_and_merge(block_2.clone());
        let fragments_2 = Eip4844BlobEncoder.encode(bundle_2).unwrap();

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
                tx_per_block: 1,
                size_per_tx: 100,
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
                tx_per_block: 1,
                size_per_tx: 100,
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

        let ImportedBlocks {
            fuel_blocks: blocks,
            ..
        } = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=3,
                tx_per_block: 1,
                size_per_tx: 100,
            })
            .await;

        let lookback_window = 2;
        let latest_height = 5u32;

        let starting_height = latest_height.saturating_sub(lookback_window);

        let blocks_to_bundle: Vec<_> = blocks
            .iter()
            .filter(|block| block.header.height >= starting_height)
            .cloned()
            .collect();

        assert_eq!(
            blocks_to_bundle.len(),
            1,
            "Expected only one block to be within the lookback window"
        );
        assert_eq!(
            blocks_to_bundle[0].header.height, 3,
            "Expected block at height 3 to be within the lookback window"
        );

        // Encode the blocks to be bundled
        let data = encode_and_merge(NonEmpty::from_vec(blocks_to_bundle.clone()).unwrap());
        let expected_fragments = Eip4844BlobEncoder.encode(data).unwrap();

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
}
