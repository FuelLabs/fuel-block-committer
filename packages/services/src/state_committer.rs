use std::{num::NonZeroUsize, time::Duration};

use itertools::Itertools;
use ports::{
    clock::Clock,
    storage::{BundleFragment, Storage},
    types::{CollectNonEmpty, DateTime, NonEmpty, Utc},
};

use crate::{Result, Runner};

// src/config.rs
#[derive(Debug, Clone)]
pub struct Config {
    /// The lookback window in blocks to determine the starting height.
    pub lookback_window: u32,
    pub fragment_accumulation_timeout: Duration,
    pub fragments_to_accumulate: NonZeroUsize,
}

#[cfg(test)]
impl Default for Config {
    fn default() -> Self {
        Self {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(0),
            fragments_to_accumulate: 1.try_into().unwrap(),
        }
    }
}

/// The `StateCommitter` is responsible for committing state fragments to L1.
pub struct StateCommitter<L1, F, Storage, C> {
    l1_adapter: L1,
    fuel_api: F,
    storage: Storage,
    config: Config,
    clock: C,
    startup_time: DateTime<Utc>,
}

impl<L1, F, Storage, C> StateCommitter<L1, F, Storage, C>
where
    C: Clock,
{
    /// Creates a new `StateCommitter`.
    pub fn new(l1_adapter: L1, fuel_api: F, storage: Storage, config: Config, clock: C) -> Self {
        let startup_time = clock.now();
        Self {
            l1_adapter,
            fuel_api,
            storage,
            config,
            clock,
            startup_time,
        }
    }
}

impl<L1, F, Db, C> StateCommitter<L1, F, Db, C>
where
    L1: ports::l1::Api,
    F: ports::fuel::Api,
    Db: Storage,
    C: Clock,
{
    async fn submit_fragments(&self, fragments: NonEmpty<BundleFragment>) -> Result<()> {
        let data = fragments
            .iter()
            .map(|f| f.fragment.clone())
            .collect_nonempty()
            .expect("non-empty vec");

        match self.l1_adapter.submit_state_fragments(data).await {
            Ok(submittal_report) => {
                let fragment_ids = fragments
                    .iter()
                    .map(|f| f.id)
                    .take(submittal_report.num_fragments.get())
                    .collect_nonempty()
                    .expect("non-empty vec");

                let ids = fragment_ids
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

    async fn next_fragments_to_submit(&self) -> Result<Option<NonEmpty<BundleFragment>>> {
        let latest_height = self.fuel_api.latest_height().await?;

        let starting_height = latest_height.saturating_sub(self.config.lookback_window);

        let existing_fragments = self
            .storage
            .oldest_nonfinalized_fragments(starting_height, 6)
            .await?;

        Ok(NonEmpty::collect(existing_fragments))
    }
}

impl<L1, F, Db, C> Runner for StateCommitter<L1, F, Db, C>
where
    F: ports::fuel::Api + Send + Sync,
    L1: ports::l1::Api + Send + Sync,
    Db: Storage + Clone + Send + Sync,
    C: Clock + Send + Sync,
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
    use clock::TestClock;
    use ports::{l1::FragmentsSubmitted, types::nonempty};

    use super::*;
    use crate::{test_utils, test_utils::mocks::l1::TxStatus, Runner, StateCommitter};

    #[tokio::test]
    async fn wont_send_fragments_if_lookback_window_moved_on() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let _expired_fragments = setup.insert_fragments(0, 3).await;
        let new_fragments = setup.insert_fragments(1, 3).await;

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([(
            Some(NonEmpty::from_vec(new_fragments.clone()).unwrap()),
            [0; 32],
        )]);

        let fuel_mock = test_utils::mocks::fuel::latest_height_is(2);
        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            fuel_mock,
            setup.db(),
            Config {
                lookback_window: 1,
                ..Default::default()
            },
            TestClock::default(),
        );

        // when
        state_committer.run().await?;

        // then
        // Mocks validate that the fragments have been sent

        Ok(())
    }

    #[tokio::test]
    async fn sends_fragments_in_order() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let fragments = setup.insert_fragments(0, 7).await;

        let first_tx_fragments = fragments[0..6].iter().cloned().collect_nonempty().unwrap();

        let second_tx_fragments = nonempty![fragments[6].clone()];
        let fragment_tx_ids = [[0; 32], [1; 32]];

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (Some(first_tx_fragments), fragment_tx_ids[0]),
            (Some(second_tx_fragments), fragment_tx_ids[1]),
        ]);

        let fuel_mock = test_utils::mocks::fuel::latest_height_is(0);
        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            fuel_mock,
            setup.db(),
            Config::default(),
            TestClock::default(),
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

        let fragments = NonEmpty::collect(setup.insert_fragments(0, 2).await).unwrap();

        let original_tx = [0; 32];
        let retry_tx = [1; 32];

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (Some(fragments.clone()), original_tx),
            (Some(fragments.clone()), retry_tx),
        ]);

        let fuel_mock = test_utils::mocks::fuel::latest_height_is(0);
        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            fuel_mock,
            setup.db(),
            Config::default(),
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
    async fn does_nothing_if_there_are_pending_transactions() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        setup.insert_fragments(0, 2).await;

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

        let fuel_mock = test_utils::mocks::fuel::latest_height_is(0);
        let mut state_committer = StateCommitter::new(
            l1_mock_submit,
            fuel_mock,
            setup.db(),
            Config::default(),
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
    async fn handles_l1_adapter_submission_failure() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        // Import enough blocks to create a bundle
        setup.insert_fragments(0, 1).await;

        // Configure the L1 adapter to fail on submission
        let mut l1_mock = ports::l1::MockApi::new();
        l1_mock.expect_submit_state_fragments().return_once(|_| {
            Box::pin(async { Err(ports::l1::Error::Other("Submission failed".into())) })
        });

        let fuel_mock = test_utils::mocks::fuel::latest_height_is(0);
        let mut state_committer = StateCommitter::new(
            l1_mock,
            fuel_mock,
            setup.db(),
            Config::default(),
            TestClock::default(),
        );

        // when
        let result = state_committer.run().await;

        // then
        assert!(result.is_err());

        Ok(())
    }
}
