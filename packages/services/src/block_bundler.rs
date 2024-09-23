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
    pub starting_fuel_height: u32,
}

#[cfg(test)]
impl Default for Config {
    fn default() -> Self {
        Self {
            optimization_time_limit: Duration::from_secs(100),
            block_accumulation_time_limit: Duration::from_secs(100),
            num_blocks_to_accumulate: NonZeroUsize::new(1).unwrap(),
            starting_fuel_height: 0,
        }
    }
}

/// The `BlockBundler` is responsible for committing state fragments to L1.
/// It bundles blocks, fragments them, and submits the fragments to the L1 adapter.
pub struct BlockBundler<Storage, Clock, BundlerFactory> {
    storage: Storage,
    clock: Clock,
    component_created_at: DateTime<Utc>,
    bundler_factory: BundlerFactory,
    config: Config,
}

impl<Storage, C, BF> BlockBundler<Storage, C, BF>
where
    C: Clock,
{
    /// Creates a new `BlockBundler`.
    pub fn new(storage: Storage, clock: C, bundler_factory: BF, config: Config) -> Self {
        let now = clock.now();

        Self {
            storage,
            clock,
            component_created_at: now,
            bundler_factory,
            config,
        }
    }
}

impl<Db, C, BF> BlockBundler<Db, C, BF>
where
    Db: Storage,
    C: Clock,
    BF: BundlerFactory,
{
    async fn bundle_and_fragment_blocks(&self) -> Result<()> {
        let Some(blocks) = self
            .storage
            .lowest_sequence_of_unbundled_blocks(
                self.config.starting_fuel_height,
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
            block_heights,
            known_to_be_optimal: optimal,
            compression_ratio,
            gas_usage,
            optimization_attempts,
        } = self.find_optimal_bundle(bundler).await?;

        info!("Bundler proposed: known_to_be_optimal={optimal}, optimization_attempts={optimization_attempts}, compression_ratio={compression_ratio}, heights={block_heights:?}, num_blocks={}, num_fragments={}, gas_usage={gas_usage:?}",  block_heights.clone().count(), fragments.len());

        self.storage
            .insert_bundle_and_fragments(block_heights, fragments)
            .await?;

        Ok(())
    }

    /// Finds the optimal bundle based on the current state and time constraints.
    async fn find_optimal_bundle<B: Bundle>(&self, mut bundler: B) -> Result<BundleProposal> {
        let optimization_start = self.clock.now();

        while bundler.advance().await? {
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

impl<Db, C, BF> Runner for BlockBundler<Db, C, BF>
where
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
    use super::*;
    use crate::test_utils::{self, encode_and_merge, Blocks, ImportedBlocks};
    use crate::CompressionLevel;
    use clock::TestClock;
    use eth::Eip4844BlobEncoder;
    use itertools::Itertools;
    use ports::l1::FragmentEncoder;
    use ports::storage::SequentialFuelBlocks;
    use ports::types::{nonempty, CollectNonEmpty, Fragment};
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
    use tokio::sync::Mutex;

    /// Define a TestBundlerWithControl that uses channels to control bundle proposals
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
        async fn advance(&mut self) -> Result<bool> {
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

        let mut block_bundler = BlockBundler::new(
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config {
                num_blocks_to_accumulate,
                ..Config::default()
            },
        );

        // when
        block_bundler.run().await?;

        // then
        assert!(setup
            .db()
            .oldest_nonfinalized_fragments(1)
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
        let data = encode_and_merge(blocks).await;
        let expected_fragments = Eip4844BlobEncoder.encode(data).unwrap();

        let clock = TestClock::default();
        let mut block_bundler = BlockBundler::new(
            setup.db(),
            clock.clone(),
            default_bundler_factory(),
            Config {
                block_accumulation_time_limit: Duration::from_secs(1),
                num_blocks_to_accumulate: 2.try_into().unwrap(),
                ..Default::default()
            },
        );

        clock.advance_time(Duration::from_secs(2));

        // when
        block_bundler.run().await?;

        // then
        let fragments = setup
            .db()
            .oldest_nonfinalized_fragments(1)
            .await?
            .into_iter()
            .map(|f| f.fragment)
            .collect_nonempty()
            .unwrap();

        assert_eq!(fragments, expected_fragments);

        assert!(setup
            .db()
            .lowest_sequence_of_unbundled_blocks(0, 1)
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
        let data = encode_and_merge(fuel_blocks).await;
        let expected_fragments = Eip4844BlobEncoder.encode(data).unwrap();

        let mut state_committer = BlockBundler::new(
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
        state_committer.run().await?;

        // then
        // we will bundle and fragment because the time limit (10s) is measured from the last finalized fragment

        let unsubmitted_fragments = setup
            .db()
            .oldest_nonfinalized_fragments(1)
            .await?
            .into_iter()
            .map(|f| f.fragment)
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

        let first_two_blocks = blocks.into_iter().take(2).collect_nonempty().unwrap();
        let bundle_data = test_utils::encode_and_merge(first_two_blocks).await;
        let fragments = Eip4844BlobEncoder.encode(bundle_data).unwrap();

        let mut state_committer = BlockBundler::new(
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config {
                num_blocks_to_accumulate: 2.try_into().unwrap(),
                ..Default::default()
            },
        );

        // when
        state_committer.run().await?;

        // then
        let unsubmitted_fragments = setup
            .db()
            .oldest_nonfinalized_fragments(10)
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
        let bundle_1 = test_utils::encode_and_merge(block_1).await;
        let fragments_1 = Eip4844BlobEncoder.encode(bundle_1).unwrap();

        let block_2 = nonempty![blocks.last().clone()];
        let bundle_2 = test_utils::encode_and_merge(block_2).await;
        let fragments_2 = Eip4844BlobEncoder.encode(bundle_2).unwrap();

        let mut bundler = BlockBundler::new(
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
        let unsubmitted_fragments = setup.db().oldest_nonfinalized_fragments(usize::MAX).await?;
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
            block_heights: 0..=0,
            known_to_be_optimal: false,
            compression_ratio: 1.0,
            gas_usage: 100,
            optimization_attempts: 10,
        };

        let (bundler_factory, send_can_advance_permission, mut notify_has_advanced) =
            ControllableBundlerFactory::setup(Some(unoptimal_bundle));

        let test_clock = TestClock::default();

        let optimization_timeout = Duration::from_secs(1);
        let mut state_committer = BlockBundler::new(
            setup.db(),
            test_clock.clone(),
            bundler_factory,
            Config {
                optimization_time_limit: optimization_timeout,
                ..Config::default()
            },
        );

        let state_committer_handle = tokio::spawn(async move {
            state_committer.run().await.unwrap();
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
        state_committer_handle.await.unwrap();

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
        let mut state_committer = BlockBundler::new(
            setup.db(),
            test_clock.clone(),
            bundler_factory,
            Config {
                optimization_time_limit: optimization_timeout,
                ..Config::default()
            },
        );

        // Spawn the BlockBundler run method in a separate task
        let state_committer_handle = tokio::spawn(async move {
            state_committer.run().await.unwrap();
        });

        // Advance the clock but not beyond the optimization time limit
        test_clock.advance_time(Duration::from_millis(500));

        // when
        for _ in 0..100 {
            send_can_advance.send(()).unwrap();
        }
        // then
        let res = tokio::time::timeout(Duration::from_millis(500), state_committer_handle).await;

        assert!(res.is_err(), "expected a timeout");

        Ok(())
    }

    fn default_bundler_factory() -> bundler::Factory<Eip4844BlobEncoder> {
        bundler::Factory::new(Eip4844BlobEncoder, CompressionLevel::Disabled)
    }
}
