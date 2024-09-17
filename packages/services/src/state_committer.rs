use std::time::Duration;

use async_trait::async_trait;
use bundler::{Bundle, BundleProposal, BundlerFactory};
use ports::{
    clock::Clock,
    storage::{BundleFragment, Storage},
    types::{DateTime, NonEmptyVec, Utc},
};

use crate::{Error, Result, Runner};

pub mod bundler;

/// Configuration for bundle generation.
#[derive(Debug, Clone, Copy)]
pub struct BundleGenerationConfig {
    /// Duration after which optimization attempts should stop.
    pub stop_optimization_attempts_after: Duration,
}

/// The `StateCommitter` is responsible for committing state fragments to L1.
/// It bundles blocks, fragments them, and submits the fragments to the L1 adapter.
pub struct StateCommitter<L1, Storage, Clock, BundlerFactory> {
    l1_adapter: L1,
    storage: Storage,
    clock: Clock,
    component_created_at: DateTime<Utc>,
    bundler_factory: BundlerFactory,
    bundle_generation_config: BundleGenerationConfig,
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
        bundle_generation_config: BundleGenerationConfig,
    ) -> Self {
        let now = clock.now();

        Self {
            l1_adapter,
            storage,
            clock,
            component_created_at: now,
            bundler_factory,
            bundle_generation_config,
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
        let bundler = self.bundler_factory.build().await?;

        let proposal = self.find_optimal_bundle(bundler).await?;

        if let Some(BundleProposal {
            fragments,
            block_heights,
            ..
        }) = proposal
        {
            let fragments = self
                .storage
                .insert_bundle_and_fragments(block_heights, fragments)
                .await?;
            Ok(Some(fragments))
        } else {
            Ok(None)
        }
    }

    /// Finds the optimal bundle based on the current state and time constraints.
    async fn find_optimal_bundle<B: Bundle>(
        &self,
        mut bundler: B,
    ) -> Result<Option<BundleProposal>> {
        while bundler.advance().await? {
            let elapsed = self.elapsed_time_since_last_finalized().await?;

            if self.should_stop_optimizing(elapsed) {
                break;
            }
        }

        bundler.finish().await
    }

    /// Calculates the elapsed time since the last finalized fragment or component creation.
    async fn elapsed_time_since_last_finalized(&self) -> Result<Duration> {
        let last_finalized_time = self
            .storage
            .last_time_a_fragment_was_finalized()
            .await?
            .unwrap_or_else(|| {
                eprintln!("No finalized fragment found; using component creation time.");
                self.component_created_at
            });
        let now = self.clock.now();
        now.signed_duration_since(last_finalized_time)
            .to_std()
            .map_err(|e| Error::Other(format!("could not calculate elapsed time: {:?}", e)))
    }

    /// Determines whether to stop optimizing based on the elapsed time.
    fn should_stop_optimizing(&self, elapsed: Duration) -> bool {
        elapsed
            >= self
                .bundle_generation_config
                .stop_optimization_attempts_after
    }

    /// Submits a fragment to the L1 adapter and records the tx in storage.
    async fn submit_fragment(&self, fragment: BundleFragment) -> Result<()> {
        match self.l1_adapter.submit_l2_state(fragment.data.clone()).await {
            Ok(tx_hash) => {
                self.storage.record_pending_tx(tx_hash, fragment.id).await?;
                tracing::info!("Submitted fragment {:?} with tx {:?}", fragment.id, tx_hash);
                Ok(())
            }
            Err(e) => {
                tracing::error!("Failed to submit fragment {:?}: {:?}", fragment.id, e);
                Err(e.into())
            }
        }
    }

    async fn has_pending_transactions(&self) -> Result<bool> {
        self.storage.has_pending_txs().await.map_err(|e| e.into())
    }

    async fn next_fragment_to_submit(&self) -> Result<Option<BundleFragment>> {
        let fragment = if let Some(fragment) = self.storage.oldest_nonfinalized_fragment().await? {
            Some(fragment)
        } else {
            self.bundle_and_fragment_blocks()
                .await?
                .map(|fragments| fragments.take_first())
        };

        Ok(fragment)
    }
}

#[async_trait]
impl<L1, Db, C, BF> Runner for StateCommitter<L1, Db, C, BF>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage + Clone + Send + Sync,
    C: Clock + Send + Sync,
    BF: BundlerFactory + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        if self.has_pending_transactions().await? {
            tracing::info!("Pending transactions detected; skipping this run.");
            return Ok(());
        }

        if let Some(fragment) = self.next_fragment_to_submit().await? {
            self.submit_fragment(fragment).await?;
        } else {
            tracing::info!("No fragments to submit at this time.");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::test_utils::mocks::l1::TxStatus;
    use crate::test_utils::Blocks;
    use crate::{test_utils, Runner, StateCommitter};
    use bundler::Compressor;
    use clock::TestClock;
    use fuel_crypto::SecretKey;
    use itertools::Itertools;
    use ports::{non_empty_vec, types::NonEmptyVec};
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

    #[async_trait::async_trait]
    impl Bundle for ControllableBundler {
        async fn advance(&mut self) -> Result<bool> {
            self.can_advance.recv().await.unwrap();
            self.notify_advanced.send(()).unwrap();
            Ok(true)
        }

        async fn finish(self) -> Result<Option<BundleProposal>> {
            Ok(Some(self.proposal.expect(
                "proposal to be set inside controllable bundler if it ever was meant to finish",
            )))
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

    #[async_trait::async_trait]
    impl BundlerFactory for ControllableBundlerFactory {
        type Bundler = ControllableBundler;

        async fn build(&self) -> Result<Self::Bundler> {
            Ok(self.bundler.lock().await.take().unwrap())
        }
    }

    #[tokio::test]
    async fn sends_fragments_in_order() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let fragment_tx_ids = [[0; 32], [1; 32]];

        let fragment_0 = test_utils::random_data(100);
        let fragment_1 = test_utils::random_data(100);

        let l1_mock_split = test_utils::mocks::l1::will_ask_to_split_bundle_into_fragments(
            None,
            non_empty_vec![fragment_0.clone(), fragment_1.clone()],
        );

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            1..2,
            Compressor::default(),
        )?;

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (fragment_0.clone(), fragment_tx_ids[0]),
            (fragment_1.clone(), fragment_tx_ids[1]),
        ]);

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            setup.db(),
            TestClock::default(),
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
            },
        );

        setup.import_blocks(Blocks::WithHeights(0..1)).await;

        // when
        // Send the first fragment
        state_committer.run().await?;
        setup
            .report_txs_finished([(fragment_tx_ids[0], TxStatus::Success)])
            .await;

        // Send the second fragment
        state_committer.run().await?;

        // then
        // Mocks validate that the fragments have been sent in order.

        Ok(())
    }

    #[tokio::test]
    async fn repeats_failed_fragments() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        setup.import_blocks(Blocks::WithHeights(0..1)).await;

        let original_tx = [0; 32];
        let retry_tx = [1; 32];

        let fragment_0 = test_utils::random_data(100);
        let fragment_1 = test_utils::random_data(100);

        let l1_mock_split = test_utils::mocks::l1::will_ask_to_split_bundle_into_fragments(
            None,
            non_empty_vec![fragment_0.clone(), fragment_1],
        );

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            1..2,
            Compressor::default(),
        )?;

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (fragment_0.clone(), original_tx),
            (fragment_0.clone(), retry_tx),
        ]);

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            setup.db(),
            TestClock::default(),
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
            },
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
        setup.import_blocks(Blocks::WithHeights(0..1)).await;

        // Configure the bundler with a minimum acceptable block range greater than the available blocks
        let min_acceptable_blocks = 2;
        let bundler_factory = bundler::Factory::new(
            Arc::new(ports::l1::MockApi::new()),
            setup.db(),
            min_acceptable_blocks..3,
            Compressor::default(),
        )?;

        let l1_mock = ports::l1::MockApi::new();

        let mut state_committer = StateCommitter::new(
            l1_mock,
            setup.db(),
            TestClock::default(),
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
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

        setup.import_blocks(Blocks::WithHeights(0..2)).await;

        let fragment = test_utils::random_data(100);

        let l1_mock_split = test_utils::mocks::l1::will_ask_to_split_bundle_into_fragments(
            None,
            non_empty_vec![fragment.clone()],
        );

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            1..2,
            Compressor::default(),
        )?;

        let mut l1_mock_submit = ports::l1::MockApi::new();
        l1_mock_submit
            .expect_submit_l2_state()
            .once()
            .return_once(|_| Ok([1; 32]));

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            setup.db(),
            TestClock::default(),
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
            },
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
    async fn bundles_minimum_acceptable_if_no_more_blocks_available() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = (0..2)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();

        setup
            .import_blocks(Blocks::Blocks {
                blocks: blocks.clone(),
                secret_key,
            })
            .await;

        let fragment = test_utils::random_data(100);

        let l1_mock_split = test_utils::mocks::l1::will_ask_to_split_bundle_into_fragments(
            Some(test_utils::encode_merge_and_compress_blocks(&blocks).await),
            non_empty_vec![fragment.clone()],
        );

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            2..3,
            Compressor::default(),
        )?;

        let l1_mock_submit =
            test_utils::mocks::l1::expects_state_submissions([(fragment.clone(), [1; 32])]);

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            setup.db(),
            TestClock::default(),
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
            },
        );

        // when
        state_committer.run().await?;

        // then
        // Mocks validate that the bundle was comprised of two blocks.

        Ok(())
    }

    #[tokio::test]
    async fn doesnt_bundle_more_than_maximum_blocks() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = (0..3)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();

        setup
            .import_blocks(Blocks::Blocks {
                blocks: blocks.clone(),
                secret_key,
            })
            .await;

        let fragment = test_utils::random_data(100);

        let l1_mock_split = test_utils::mocks::l1::will_ask_to_split_bundle_into_fragments(
            Some(test_utils::encode_merge_and_compress_blocks(&blocks[0..2]).await),
            non_empty_vec![fragment.clone()],
        );

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            2..3,
            Compressor::default(),
        )?;

        let l1_mock_submit =
            test_utils::mocks::l1::expects_state_submissions([(fragment.clone(), [1; 32])]);

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            setup.db(),
            TestClock::default(),
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
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
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        let blocks = (0..=1)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();

        setup
            .import_blocks(Blocks::Blocks {
                blocks: blocks.clone(),
                secret_key,
            })
            .await;

        let bundle_1_tx = [0; 32];
        let bundle_2_tx = [1; 32];

        let bundle_1 = test_utils::encode_merge_and_compress_blocks(&blocks[0..=0]).await;
        let bundle_1_fragment = test_utils::random_data(100);

        let bundle_2 = test_utils::encode_merge_and_compress_blocks(&blocks[1..=1]).await;
        let bundle_2_fragment = test_utils::random_data(100);

        let l1_mock_split = test_utils::mocks::l1::will_ask_to_split_bundles_into_fragments([
            (
                Some(bundle_1.clone()),
                non_empty_vec![bundle_1_fragment.clone()],
            ),
            (
                Some(bundle_2.clone()),
                non_empty_vec![bundle_2_fragment.clone()],
            ),
        ]);

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            1..2,
            Compressor::default(),
        )?;

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (bundle_1_fragment.clone(), bundle_1_tx),
            (bundle_2_fragment.clone(), bundle_2_tx),
        ]);

        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            setup.db(),
            TestClock::default(),
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
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
    async fn stops_advancing_if_time_since_last_finalized_exceeds_threshold() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let fragment_tx_id = [2; 32];
        let unoptimal_fragment = test_utils::random_data(100);

        let unoptimal_bundle = BundleProposal {
            fragments: non_empty_vec![unoptimal_fragment.clone()],
            block_heights: 0..=0,
            optimal: false,
            compression_ratio: 1.0,
        };

        let (bundler_factory, send_can_advance_permission, mut notify_has_advanced) =
            ControllableBundlerFactory::setup(Some(unoptimal_bundle));

        let l1_mock = test_utils::mocks::l1::expects_state_submissions([(
            unoptimal_fragment.clone(),
            fragment_tx_id,
        )]);

        let test_clock = TestClock::default();

        let mut state_committer = StateCommitter::new(
            l1_mock,
            setup.db(),
            test_clock.clone(),
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
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
        test_clock.advance_time(Duration::from_secs(2));

        // Submit the final (unoptimal) bundle proposal

        send_can_advance_permission.send(()).unwrap();

        // then
        // Wait for the StateCommitter task to complete
        state_committer_handle.await.unwrap();

        // Verify that both fragments were submitted
        // Since l1_mock_submit expects two submissions, the test will fail if they weren't called

        Ok(())
    }

    #[tokio::test]
    async fn doesnt_stop_advancing_if_there_is_still_time() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let fragment_tx_id = [3; 32];

        let (bundler_factory, send_can_advance, mut notify_advanced) =
            ControllableBundlerFactory::setup(None);

        // Create a TestClock
        let test_clock = TestClock::default();

        // Create the StateCommitter
        let mut state_committer = StateCommitter::new(
            ports::l1::MockApi::new(),
            setup.db(),
            test_clock.clone(),
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
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
        let res = tokio::time::timeout(Duration::from_millis(100), state_committer_handle).await;

        assert!(res.is_err(), "expected a timeout");

        Ok(())
    }

    #[tokio::test]
    async fn stops_optimizing_bundle_if_last_finalized_fragment_happened_too_long_ago() -> Result<()>
    {
        // given
        let setup = test_utils::Setup::init().await;

        let last_finalization_time = Utc::now();
        setup
            .commit_single_block_bundle(last_finalization_time)
            .await;

        let fragment_tx_id = [3; 32];
        let unoptimal_fragment = test_utils::random_data(100);

        let (bundler_factory, unblock_bundler_advance, mut notify_advanced) =
            ControllableBundlerFactory::setup(Some(BundleProposal {
                fragments: non_empty_vec![unoptimal_fragment.clone()],
                block_heights: 1..=1,
                optimal: false,
                compression_ratio: 1.0,
            }));

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([(
            unoptimal_fragment.clone(),
            fragment_tx_id,
        )]);

        let test_clock = TestClock::default();
        let optimization_timeout = Duration::from_secs(1);
        test_clock.set_time(last_finalization_time + optimization_timeout);

        // Create the StateCommitter
        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            setup.db(),
            test_clock.clone(),
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: optimization_timeout,
            },
        );

        // Spawn the StateCommitter run method in a separate task
        let state_committer_handle = tokio::spawn(async move {
            state_committer.run().await.unwrap();
        });

        // when

        // Send the unoptimal bundle proposal
        unblock_bundler_advance.send(()).unwrap();

        notify_advanced.recv().await.unwrap();

        // then
        state_committer_handle.await.unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn handles_no_bundle_proposals_due_to_insufficient_blocks() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        // Import fewer blocks than the minimum acceptable amount
        setup.import_blocks(Blocks::WithHeights(0..1)).await;

        // Configure the bundler with a minimum acceptable block range greater than the available blocks
        let min_acceptable_blocks = 2;
        let bundler_factory = bundler::Factory::new(
            Arc::new(ports::l1::MockApi::new()),
            setup.db(),
            min_acceptable_blocks..3,
            Compressor::default(),
        )?;

        let l1_mock = ports::l1::MockApi::new();

        let mut state_committer = StateCommitter::new(
            l1_mock,
            setup.db(),
            TestClock::default(),
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
            },
        );

        // when
        state_committer.run().await?;

        // then
        // No fragments should have been submitted, and no errors should occur.

        Ok(())
    }

    #[tokio::test]
    async fn handles_l1_adapter_submission_failure() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        // Import enough blocks to create a bundle
        setup.import_blocks(Blocks::WithHeights(0..1)).await;

        let fragment = test_utils::random_data(100);
        let fragment_tx_id = [4; 32];

        let db = setup.db();

        let l1_mock = test_utils::mocks::l1::will_ask_to_split_bundle_into_fragments(
            None,
            non_empty_vec!(fragment.clone()),
        );

        let factory =
            bundler::Factory::new(Arc::new(l1_mock), db.clone(), 1..2, Compressor::default())?;

        // Configure the L1 adapter to fail on submission
        let mut l1_mock = ports::l1::MockApi::new();
        l1_mock
            .expect_submit_l2_state()
            .return_once(|_| Err(ports::l1::Error::Other("Submission failed".into())));

        let mut state_committer = StateCommitter::new(
            l1_mock,
            db,
            TestClock::default(),
            factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
            },
        );

        // when
        let result = state_committer.run().await;

        // then
        assert!(result.is_err());

        Ok(())
    }
}
