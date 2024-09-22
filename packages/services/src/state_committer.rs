use std::{num::NonZeroUsize, time::Duration};

use bundler::{Bundle, BundleProposal, BundlerFactory};
use itertools::Itertools;
use ports::{
    clock::Clock,
    storage::{BundleFragment, Storage},
    types::{DateTime, NonEmptyVec, Utc},
};
use tracing::info;

use crate::{Error, Result, Runner};

pub mod bundler;

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

/// The `StateCommitter` is responsible for committing state fragments to L1.
/// It bundles blocks, fragments them, and submits the fragments to the L1 adapter.
pub struct StateCommitter<L1, Storage, Clock, BundlerFactory> {
    l1_adapter: L1,
    storage: Storage,
    clock: Clock,
    component_created_at: DateTime<Utc>,
    bundler_factory: BundlerFactory,
    config: Config,
}

impl<L1, Storage, C, BF> StateCommitter<L1, Storage, C, BF>
where
    C: Clock,
{
    /// Creates a new `StateCommitter`.
    pub fn new(
        l1_adapter: L1,
        storage: Storage,
        clock: C,
        bundler_factory: BF,
        config: Config,
    ) -> Self {
        let now = clock.now();

        Self {
            l1_adapter,
            storage,
            clock,
            component_created_at: now,
            bundler_factory,
            config,
        }
    }
}

impl<L1, Db, C, BF> StateCommitter<L1, Db, C, BF>
where
    L1: ports::l1::Api,
    Db: Storage,
    C: Clock,
    BF: BundlerFactory,
{
    async fn bundle_and_fragment_blocks(&self) -> Result<Option<NonEmptyVec<BundleFragment>>> {
        let Some(blocks) = self
            .storage
            .lowest_sequence_of_unbundled_blocks(
                self.config.starting_fuel_height,
                self.config.num_blocks_to_accumulate.get(),
            )
            .await?
        else {
            return Ok(None);
        };

        let still_time_to_accumulate_more = self.still_time_to_accumulate_more().await?;
        if blocks.len() < self.config.num_blocks_to_accumulate && still_time_to_accumulate_more {
            info!(
                "Not enough blocks ({} < {}) to bundle. Waiting for more to accumulate.",
                blocks.len(),
                self.config.num_blocks_to_accumulate.get()
            );

            return Ok(None);
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
        } = self.find_optimal_bundle(bundler).await?;

        info!("Bundler proposed: optimal={optimal}, compression_ratio={compression_ratio}, heights={block_heights:?}, num_fragments={}, gas_usage={gas_usage:?}", fragments.len());

        let fragments = self
            .storage
            .insert_bundle_and_fragments(block_heights, fragments)
            .await?;

        Ok(Some(fragments))
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

    /// Submits a fragment to the L1 adapter and records the tx in storage.
    async fn submit_fragments(&self, fragments: NonEmptyVec<BundleFragment>) -> Result<()> {
        let data = fragments
            .inner()
            .iter()
            .map(|f| f.data.clone())
            .collect::<Vec<_>>()
            .try_into()
            .expect("non-empty vec");
        eprintln!("submitting fragments");

        match self.l1_adapter.submit_state_fragments(data).await {
            Ok(submittal_report) => {
                let fragment_ids = NonEmptyVec::try_from(
                    fragments
                        .inner()
                        .iter()
                        .map(|f| f.id)
                        .take(submittal_report.num_fragments.get())
                        .collect_vec(),
                )
                .expect("non-empty vec");

                let ids = fragment_ids
                    .inner()
                    .iter()
                    .map(|id| id.as_u32().to_string())
                    .join(", ");

                self.storage
                    .record_pending_tx(submittal_report.tx, fragment_ids)
                    .await?;

                tracing::info!(
                    "Submitted fragments {ids} with tx {}",
                    hex::encode(submittal_report.tx)
                );
                Ok(())
            }
            Err(e) => {
                let ids = fragments
                    .inner()
                    .iter()
                    .map(|f| f.id.as_u32().to_string())
                    .join(", ");

                tracing::error!("Failed to submit fragments {ids}: {e}");
                Err(e.into())
            }
        }
    }

    async fn has_pending_transactions(&self) -> Result<bool> {
        self.storage.has_pending_txs().await.map_err(|e| e.into())
    }

    async fn next_fragments_to_submit(&self) -> Result<Option<NonEmptyVec<BundleFragment>>> {
        let existing_fragments = self.storage.oldest_nonfinalized_fragments(6).await?;

        let fragments = if !existing_fragments.is_empty() {
            Some(existing_fragments.try_into().expect("non-empty vec"))
        } else {
            self.bundle_and_fragment_blocks().await?
        };

        Ok(fragments)
    }
}

impl<L1, Db, C, BF> Runner for StateCommitter<L1, Db, C, BF>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage + Clone + Send + Sync,
    C: Clock + Send + Sync,
    BF: BundlerFactory + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        if self.has_pending_transactions().await? {
            return Ok(());
        }

        if let Some(fragments) = self.next_fragments_to_submit().await? {
            self.submit_fragments(fragments).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::mocks::l1::TxStatus;
    use crate::test_utils::{Blocks, ImportedBlocks};
    use crate::{test_utils, CompressionLevel, Runner, StateCommitter};
    use clock::TestClock;
    use eth::Eip4844BlobEncoder;
    use ports::l1::{FragmentEncoder, FragmentsSubmitted};
    use ports::non_empty_vec;
    use ports::storage::SequentialFuelBlocks;
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
    async fn sends_fragments_in_order() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        // Loss due to blob encoding
        let fits_in_a_blob = (Eip4844BlobEncoder::FRAGMENT_SIZE as f64 * 0.96) as usize;
        let ImportedBlocks {
            fuel_blocks: blocks,
            ..
        } = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=0,
                tx_per_block: 7,
                size_per_tx: fits_in_a_blob,
            })
            .await;

        let bundle_data = test_utils::encode_and_merge(blocks).await;
        let expected_fragments = Eip4844BlobEncoder.encode(bundle_data).unwrap();

        assert_eq!(expected_fragments.len().get(), 7);

        let fragment_tx_ids = [[0; 32], [1; 32]];

        let first_tx_fragments = expected_fragments.clone();
        let second_tx_fragments = non_empty_vec![expected_fragments[6].clone()];

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            // We give all 7 fragments in the first submission, but 1 wont be used
            (Some(first_tx_fragments), fragment_tx_ids[0]),
            // It will be sent next time
            (Some(second_tx_fragments), fragment_tx_ids[1]),
        ]);

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config::default(),
        );

        // when
        // Send the first fragments
        state_committer.run().await?;
        setup
            .report_txs_finished([(fragment_tx_ids[0], TxStatus::Success)])
            .await;

        // Send the second fragments
        state_committer.run().await?;

        // then
        // Mocks validate that the fragments have been sent in order.

        Ok(())
    }

    #[tokio::test]
    async fn repeats_failed_fragments() -> Result<()> {
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
        let bundle_data = test_utils::encode_and_merge(blocks).await;
        let fragments = Eip4844BlobEncoder.encode(bundle_data).unwrap();

        let original_tx = [0; 32];
        let retry_tx = [1; 32];

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (Some(fragments.clone()), original_tx),
            (Some(fragments.clone()), retry_tx),
        ]);

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config::default(),
        );

        // when
        // Send the first fragment (which will fail)
        state_committer.run().await?;
        setup
            .report_txs_finished([(original_tx, TxStatus::Failure)])
            .await;

        // Retry sending the failed fragment
        state_committer.run().await?;

        // then
        // Mocks validate that the failed fragment was retried.

        Ok(())
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

        let l1_mock = ports::l1::MockApi::new();

        let mut state_committer = StateCommitter::new(
            l1_mock,
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config {
                num_blocks_to_accumulate,
                ..Config::default()
            },
        );

        // when
        state_committer.run().await?;

        // then
        // No fragments should have been submitted, and no errors should occur.

        Ok(())
    }

    #[tokio::test]
    async fn does_nothing_if_there_are_pending_transactions() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=1,
                tx_per_block: 1,
                size_per_tx: 100,
            })
            .await;

        let mut l1_mock_submit = ports::l1::MockApi::new();
        l1_mock_submit
            .expect_submit_state_fragments()
            .once()
            .return_once(|_| {
                Box::pin(async {
                    Ok(FragmentsSubmitted {
                        tx: [1; 32],
                        num_fragments: 6.try_into().unwrap(),
                    })
                })
            });

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config::default(),
        );

        // when
        // First run: bundles and sends the first fragment
        state_committer.run().await?;

        // Second run: should do nothing due to pending transaction
        state_committer.run().await?;

        // then
        // Mocks validate that no additional submissions were made.

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

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([(None, [1; 32])]);

        let clock = TestClock::default();
        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
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
        state_committer.run().await?;

        // then

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

        let ImportedBlocks {
            fuel_blocks: blocks,
            ..
        } = setup
            .import_blocks(Blocks::WithHeights {
                range: 1..=1,
                tx_per_block: 1,
                size_per_tx: 100,
            })
            .await;
        let bundle_data = test_utils::encode_and_merge(blocks).await;

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([(None, [1; 32])]);

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
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

        let bundle_data =
            test_utils::encode_and_merge(blocks.inner()[..2].to_vec().try_into().unwrap()).await;
        let fragments = Eip4844BlobEncoder.encode(bundle_data).unwrap();

        let l1_mock_submit =
            test_utils::mocks::l1::expects_state_submissions([(Some(fragments), [1; 32])]);

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
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
        // Mocks validate that only two blocks were bundled even though three were available.

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

        let bundle_1_tx = [0; 32];
        let bundle_2_tx = [1; 32];

        let bundle_1 =
            test_utils::encode_and_merge(blocks.inner()[0..=0].to_vec().try_into().unwrap()).await;
        let fragments_1 = Eip4844BlobEncoder.encode(bundle_1).unwrap();

        let bundle_2 =
            test_utils::encode_and_merge(blocks.inner()[1..=1].to_vec().try_into().unwrap()).await;
        let fragments_2 = Eip4844BlobEncoder.encode(bundle_2).unwrap();

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (Some(fragments_1), bundle_1_tx),
            (Some(fragments_2), bundle_2_tx),
        ]);

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config {
                num_blocks_to_accumulate: 1.try_into().unwrap(),
                ..Default::default()
            },
        );

        // when
        // Send the first bundle
        state_committer.run().await?;
        setup
            .report_txs_finished([(bundle_1_tx, TxStatus::Success)])
            .await;

        // Send the second bundle
        state_committer.run().await?;

        // then
        // Mocks validate that the second block was bundled and sent.

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

        let fragment_tx_id = [2; 32];
        let unoptimal_fragments = non_empty_vec![test_utils::random_data(100usize)];

        let unoptimal_bundle = BundleProposal {
            fragments: unoptimal_fragments.clone(),
            block_heights: 0..=0,
            known_to_be_optimal: false,
            compression_ratio: 1.0,
            gas_usage: 100,
        };

        let (bundler_factory, send_can_advance_permission, mut notify_has_advanced) =
            ControllableBundlerFactory::setup(Some(unoptimal_bundle));

        let l1_mock = test_utils::mocks::l1::expects_state_submissions([(
            Some(unoptimal_fragments),
            fragment_tx_id,
        )]);

        let test_clock = TestClock::default();

        let optimization_timeout = Duration::from_secs(1);
        let mut state_committer = StateCommitter::new(
            l1_mock,
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

        // Submit the final (unoptimal) bundle proposal

        send_can_advance_permission.send(()).unwrap();

        // then
        // Wait for the StateCommitter task to complete
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

        // Create the StateCommitter
        let optimization_timeout = Duration::from_secs(1);
        let mut state_committer = StateCommitter::new(
            ports::l1::MockApi::new(),
            setup.db(),
            test_clock.clone(),
            bundler_factory,
            Config {
                optimization_time_limit: optimization_timeout,
                ..Config::default()
            },
        );

        // Spawn the StateCommitter run method in a separate task
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

    #[tokio::test]
    async fn handles_l1_adapter_submission_failure() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        // Import enough blocks to create a bundle
        setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=0,
                tx_per_block: 1,
                size_per_tx: 100,
            })
            .await;

        // Configure the L1 adapter to fail on submission
        let mut l1_mock = ports::l1::MockApi::new();
        l1_mock.expect_submit_state_fragments().return_once(|_| {
            Box::pin(async { Err(ports::l1::Error::Other("Submission failed".into())) })
        });

        let mut state_committer = StateCommitter::new(
            l1_mock,
            setup.db(),
            TestClock::default(),
            default_bundler_factory(),
            Config::default(),
        );

        // when
        let result = state_committer.run().await;

        // then
        assert!(result.is_err());

        Ok(())
    }

    fn default_bundler_factory() -> bundler::Factory<Eip4844BlobEncoder> {
        bundler::Factory::new(Eip4844BlobEncoder, CompressionLevel::Disabled)
    }
}
