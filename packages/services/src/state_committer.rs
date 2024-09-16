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
        let mut bundler = self.bundler_factory.build().await?;
        let start_time = self.clock.now();

        let proposal = self.find_optimal_bundle(&mut bundler, start_time).await?;

        if let Some(BundleProposal {
            fragments,
            block_heights,
            ..
        }) = proposal
        {
            let fragments = self
                .storage
                .insert_bundle_and_fragments(block_heights, fragments.fragments)
                .await?;
            Ok(Some(fragments))
        } else {
            Ok(None)
        }
    }

    /// Finds the optimal bundle within the specified time frame.
    async fn find_optimal_bundle<B: Bundle>(
        &self,
        bundler: &mut B,
        start_time: DateTime<Utc>,
    ) -> Result<Option<BundleProposal>> {
        loop {
            if let Some(bundle) = bundler.propose_bundle().await? {
                let elapsed = self.elapsed_time_since(start_time)?;
                if bundle.optimal || self.should_stop_optimizing(elapsed) {
                    return Ok(Some(bundle));
                }
            } else {
                return Ok(None);
            }
        }
    }

    /// Calculates the elapsed time since the given start time.
    fn elapsed_time_since(&self, start_time: DateTime<Utc>) -> Result<Duration> {
        let now = self.clock.now();
        now.signed_duration_since(start_time)
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
        if let Some(fragment) = self.storage.oldest_nonfinalized_fragment().await? {
            Ok(Some(fragment))
        } else {
            Ok(self
                .bundle_and_fragment_blocks()
                .await?
                .map(|fragments| fragments.take_first()))
        }
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

    use clock::TestClock;
    use fuel_crypto::SecretKey;
    use itertools::Itertools;
    use ports::{l1::SubmittableFragments, non_empty_vec, types::NonEmptyVec};
    use tokio::sync::Mutex;

    use crate::test_utils::{self, mocks::l1::TxStatus, Blocks};

    use super::*;

    #[tokio::test]
    async fn sends_fragments_in_order() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let fragment_tx_ids = [[0; 32], [1; 32]];

        let fragment_0 = test_utils::random_data(100);
        let fragment_1 = test_utils::random_data(100);

        let l1_mock_split =
            test_utils::mocks::l1::will_split_bundle_into_fragments(SubmittableFragments {
                fragments: non_empty_vec![fragment_0.clone(), fragment_1.clone()],
                gas_estimation: 1,
            });

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            (1..2).try_into().unwrap(),
        );

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (fragment_0.clone(), fragment_tx_ids[0]),
            (fragment_1.clone(), fragment_tx_ids[1]),
        ]);

        let mut state_committer = create_state_committer(
            l1_mock_submit,
            setup.db(),
            bundler_factory,
            TestClock::default(),
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

        let l1_mock_split =
            test_utils::mocks::l1::will_split_bundle_into_fragments(SubmittableFragments {
                fragments: non_empty_vec![fragment_0.clone(), fragment_1],
                gas_estimation: 1,
            });

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            (1..2).try_into().unwrap(),
        );

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (fragment_0.clone(), original_tx),
            (fragment_0.clone(), retry_tx),
        ]);

        let mut state_committer = create_state_committer(
            l1_mock_submit,
            setup.db(),
            bundler_factory,
            TestClock::default(),
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

        let l1_mock = ports::l1::MockApi::new();

        let bundler_factory = bundler::Factory::new(
            Arc::new(ports::l1::MockApi::new()),
            setup.db(),
            (2..3).try_into().unwrap(),
        );

        let mut state_committer =
            create_state_committer(l1_mock, setup.db(), bundler_factory, TestClock::default());

        // when
        state_committer.run().await?;

        // then
        // Mocks will validate that nothing happened.

        Ok(())
    }

    #[tokio::test]
    async fn does_nothing_if_there_are_pending_transactions() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        setup.import_blocks(Blocks::WithHeights(0..2)).await;

        let fragment = test_utils::random_data(100);

        let l1_mock_split =
            test_utils::mocks::l1::will_split_bundle_into_fragments(SubmittableFragments {
                fragments: non_empty_vec![fragment.clone()],
                gas_estimation: 1,
            });

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            (1..2).try_into().unwrap(),
        );

        let mut l1_mock_submit = ports::l1::MockApi::new();
        l1_mock_submit
            .expect_submit_l2_state()
            .once()
            .return_once(|_| Ok([1; 32]));

        let mut state_committer = create_state_committer(
            l1_mock_submit,
            setup.db(),
            bundler_factory,
            TestClock::default(),
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

        let l1_mock_split = test_utils::mocks::l1::will_split_bundles_into_fragments([(
            test_utils::encode_merge_and_compress_blocks(&blocks).await,
            SubmittableFragments {
                fragments: non_empty_vec![fragment.clone()],
                gas_estimation: 1,
            },
        )]);

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            (2..3).try_into().unwrap(),
        );

        let l1_mock_submit =
            test_utils::mocks::l1::expects_state_submissions([(fragment.clone(), [1; 32])]);

        let mut state_committer = create_state_committer(
            l1_mock_submit,
            setup.db(),
            bundler_factory,
            TestClock::default(),
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

        let l1_mock_split = test_utils::mocks::l1::will_split_bundles_into_fragments([(
            test_utils::encode_merge_and_compress_blocks(&blocks[0..2]).await,
            SubmittableFragments {
                fragments: non_empty_vec![fragment.clone()],
                gas_estimation: 1,
            },
        )]);

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            (2..3).try_into().unwrap(),
        );

        let l1_mock_submit =
            test_utils::mocks::l1::expects_state_submissions([(fragment.clone(), [1; 32])]);

        let mut state_committer = create_state_committer(
            l1_mock_submit,
            setup.db(),
            bundler_factory,
            TestClock::default(),
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

        let l1_mock_split = test_utils::mocks::l1::will_split_bundles_into_fragments([
            (
                bundle_1.clone(),
                SubmittableFragments {
                    fragments: non_empty_vec![bundle_1_fragment.clone()],
                    gas_estimation: 1,
                },
            ),
            (
                bundle_2.clone(),
                SubmittableFragments {
                    fragments: non_empty_vec![bundle_2_fragment.clone()],
                    gas_estimation: 1,
                },
            ),
        ]);

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            (1..2).try_into().unwrap(),
        );

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (bundle_1_fragment.clone(), bundle_1_tx),
            (bundle_2_fragment.clone(), bundle_2_tx),
        ]);

        let mut state_committer = create_state_committer(
            l1_mock_submit,
            setup.db(),
            bundler_factory,
            TestClock::default(),
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
    async fn can_be_disabled_by_empty_acceptable_block_range() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let bundler_factory = bundler::Factory::new(
            Arc::new(ports::l1::MockApi::new()),
            setup.db(),
            (0..1).try_into().unwrap(),
        );

        let mut state_committer = create_state_committer(
            ports::l1::MockApi::new(),
            setup.db(),
            bundler_factory,
            TestClock::default(),
        );

        // when
        state_committer.run().await?;

        // then
        // No calls to mocks were made.

        Ok(())
    }

    #[tokio::test]
    async fn optimizes_for_gas_per_byte() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        let blocks = (0..=3)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();

        setup
            .import_blocks(Blocks::Blocks {
                blocks: blocks.clone(),
                secret_key,
            })
            .await;

        let bundle_1 = test_utils::encode_merge_and_compress_blocks(&blocks[0..=1]).await; // 2 blocks
        let bundle_2 = test_utils::encode_merge_and_compress_blocks(&blocks[0..=2]).await; // 3 blocks (best gas per byte)
        let bundle_3 = test_utils::encode_merge_and_compress_blocks(&blocks[0..=3]).await; // 4 blocks

        let optimal_fragment = test_utils::random_data(100);

        let l1_mock_split = test_utils::mocks::l1::will_split_bundles_into_fragments([
            (
                bundle_1,
                SubmittableFragments {
                    fragments: non_empty_vec![test_utils::random_data(100)], // 100 bytes, gas estimation 2
                    gas_estimation: 2,
                },
            ),
            (
                bundle_2,
                SubmittableFragments {
                    fragments: non_empty_vec![optimal_fragment.clone()], // 100 bytes, gas estimation 1 (best gas per byte)
                    gas_estimation: 1,
                },
            ),
            (
                bundle_3,
                SubmittableFragments {
                    fragments: non_empty_vec![test_utils::random_data(100)], // 100 bytes, gas estimation 3
                    gas_estimation: 3,
                },
            ),
        ]);

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            (2..5).try_into().unwrap(), // Valid block range: 2 to 4 blocks
        );

        let l1_mock_submit =
            test_utils::mocks::l1::expects_state_submissions([(optimal_fragment.clone(), [0; 32])]);

        let mut state_committer = create_state_committer(
            l1_mock_submit,
            setup.db(),
            bundler_factory,
            TestClock::default(),
        );

        state_committer.run().await?;

        // Then: Validate that the bundle with the best gas per byte was chosen
        // Mocks validate that the bundle with the best gas per byte (bundle_2) was submitted

        Ok(())
    }

    #[tokio::test]
    async fn stops_optimizing_if_time_exhausted() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        struct TestBundler {
            rx: tokio::sync::mpsc::Receiver<Option<BundleProposal>>,
            notify_consumed: tokio::sync::mpsc::Sender<()>,
        }

        #[async_trait::async_trait]
        impl Bundle for TestBundler {
            async fn propose_bundle(&mut self) -> Result<Option<BundleProposal>> {
                let bundle = self.rx.recv().await.unwrap_or(None);
                self.notify_consumed.send(()).await.unwrap();
                Ok(bundle)
            }
        }

        struct TestBundlerFactory {
            bundler: Mutex<Option<TestBundler>>,
        }

        #[async_trait::async_trait]
        impl BundlerFactory for TestBundlerFactory {
            type Bundler = TestBundler;

            async fn build(&self) -> Result<Self::Bundler> {
                Ok(self.bundler.lock().await.take().unwrap())
            }
        }

        let (send_bundles, receive_bundles) = tokio::sync::mpsc::channel(1);
        let (send_consumed, mut receive_consumed) = tokio::sync::mpsc::channel(1);
        let test_bundler = TestBundler {
            rx: receive_bundles,
            notify_consumed: send_consumed,
        };

        let bundler_factory = TestBundlerFactory {
            bundler: Mutex::new(Some(test_bundler)),
        };

        let test_clock = TestClock::default();
        let final_fragment = non_empty_vec![1];

        let l1_mock_submit =
            test_utils::mocks::l1::expects_state_submissions([(final_fragment.clone(), [0; 32])]);

        let mut state_committer = create_state_committer(
            l1_mock_submit,
            setup.db(),
            bundler_factory,
            test_clock.clone(),
        );

        let state_committer_task = tokio::task::spawn(async move {
            state_committer.run().await.unwrap();
        });

        // when
        // Send the first (non-optimal) bundle proposal
        send_bundles
            .send(Some(BundleProposal {
                fragments: SubmittableFragments {
                    fragments: non_empty_vec![non_empty_vec![0]],
                    gas_estimation: 1,
                },
                block_heights: (0..1).try_into().unwrap(),
                optimal: false,
                compression_ratio: 1.0,
            }))
            .await
            .unwrap();

        receive_consumed.recv().await.unwrap();

        // Advance the clock to exceed the optimization time limit
        test_clock.advance_time(Duration::from_secs(1)).await;

        // Send the second bundle proposal
        send_bundles
            .send(Some(BundleProposal {
                fragments: SubmittableFragments {
                    fragments: non_empty_vec![final_fragment.clone()],
                    gas_estimation: 1,
                },
                block_heights: (0..1).try_into().unwrap(),
                optimal: false,
                compression_ratio: 1.0,
            }))
            .await
            .unwrap();

        // then
        // The state committer should stop optimization and proceed with the best proposal
        tokio::time::timeout(Duration::from_secs(1), state_committer_task)
            .await
            .unwrap()
            .unwrap();

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
            (min_acceptable_blocks..3).try_into().unwrap(),
        );

        let l1_mock = ports::l1::MockApi::new();

        let mut state_committer =
            create_state_committer(l1_mock, setup.db(), bundler_factory, TestClock::default());

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

        // Configure the L1 adapter to fail on submission
        let mut l1_mock = ports::l1::MockApi::new();
        l1_mock
            .expect_submit_l2_state()
            .return_once(|_| Err(ports::l1::Error::Other("Submission failed".into())));

        // Use the actual storage and bundler
        let l1_mock_split =
            test_utils::mocks::l1::will_split_bundle_into_fragments(SubmittableFragments {
                fragments: non_empty_vec![fragment.clone()],
                gas_estimation: 1,
            });

        let bundler_factory = bundler::Factory::new(
            Arc::new(l1_mock_split),
            setup.db(),
            (1..2).try_into().unwrap(),
        );

        let mut state_committer =
            create_state_committer(l1_mock, setup.db(), bundler_factory, TestClock::default());

        // when
        let result = state_committer.run().await;

        // then
        assert!(
            result.is_err(),
            "Expected an error due to L1 submission failure"
        );

        Ok(())
    }

    fn create_state_committer<L1>(
        l1_adapter: L1,
        storage: impl Storage + Clone,
        bundler_factory: impl BundlerFactory,
        clock: impl Clock,
    ) -> StateCommitter<L1, impl Storage + Clone, impl Clock, impl BundlerFactory> {
        StateCommitter::new(
            l1_adapter,
            storage,
            clock,
            bundler_factory,
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
            },
        )
    }
}
