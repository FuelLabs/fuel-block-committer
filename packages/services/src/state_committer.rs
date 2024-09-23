use itertools::Itertools;
use ports::{
    storage::{BundleFragment, Storage},
    types::{CollectNonEmpty, NonEmpty},
};

use crate::{Result, Runner};

/// The `StateCommitter` is responsible for committing state fragments to L1.
pub struct StateCommitter<L1, Storage> {
    l1_adapter: L1,
    storage: Storage,
}

impl<L1, Storage> StateCommitter<L1, Storage> {
    /// Creates a new `StateCommitter`.
    pub fn new(l1_adapter: L1, storage: Storage) -> Self {
        Self {
            l1_adapter,
            storage,
        }
    }
}

impl<L1, Db> StateCommitter<L1, Db>
where
    L1: ports::l1::Api,
    Db: Storage,
{
    /// Submits a fragment to the L1 adapter and records the tx in storage.
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
        let existing_fragments = self.storage.oldest_nonfinalized_fragments(6).await?;

        Ok(NonEmpty::collect(existing_fragments))
    }
}

impl<L1, Db> Runner for StateCommitter<L1, Db>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage + Clone + Send + Sync,
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
    use crate::{test_utils, Runner, StateCommitter};

    use ports::l1::FragmentsSubmitted;
    use ports::types::nonempty;

    #[tokio::test]
    async fn sends_fragments_in_order() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let fragments = setup.insert_fragments(7).await;

        let first_tx_fragments = fragments[0..6].iter().cloned().collect_nonempty().unwrap();

        let second_tx_fragments = nonempty![fragments[6].clone()];
        let fragment_tx_ids = [[0; 32], [1; 32]];

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (Some(first_tx_fragments), fragment_tx_ids[0]),
            (Some(second_tx_fragments), fragment_tx_ids[1]),
        ]);

        let mut state_committer = StateCommitter::new(l1_mock_submit, setup.db());

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

        let fragments = NonEmpty::collect(setup.insert_fragments(2).await).unwrap();

        let original_tx = [0; 32];
        let retry_tx = [1; 32];

        let l1_mock_submit = test_utils::mocks::l1::expects_state_submissions([
            (Some(fragments.clone()), original_tx),
            (Some(fragments.clone()), retry_tx),
        ]);

        let mut state_committer = StateCommitter::new(l1_mock_submit, setup.db());

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

        setup.insert_fragments(2).await;

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

        let mut state_committer = StateCommitter::new(l1_mock_submit, setup.db());

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
        setup.insert_fragments(1).await;

        // Configure the L1 adapter to fail on submission
        let mut l1_mock = ports::l1::MockApi::new();
        l1_mock.expect_submit_state_fragments().return_once(|_| {
            Box::pin(async { Err(ports::l1::Error::Other("Submission failed".into())) })
        });

        let mut state_committer = StateCommitter::new(l1_mock, setup.db());

        // when
        let result = state_committer.run().await;

        // then
        assert!(result.is_err());

        Ok(())
    }
}
