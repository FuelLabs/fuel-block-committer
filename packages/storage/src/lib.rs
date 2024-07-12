#![deny(unused_crate_dependencies)]
mod tables;
#[cfg(feature = "test-helpers")]
mod test_instance;
#[cfg(feature = "test-helpers")]
pub use test_instance::*;

mod error;
mod postgres;
use ports::types::BlockSubmission;
pub use postgres::*;

use ports::types::{StateFragment, StateFragmentId, StateSubmission};

#[async_trait::async_trait]
impl ports::storage::Storage for postgres::Postgres {
    async fn insert(&self, submission: BlockSubmission) -> ports::storage::Result<()> {
        Ok(self._insert(submission).await?)
    }

    async fn submission_w_latest_block(&self) -> ports::storage::Result<Option<BlockSubmission>> {
        Ok(self._submission_w_latest_block().await?)
    }

    async fn set_submission_completed(
        &self,
        fuel_block_hash: [u8; 32],
    ) -> ports::storage::Result<BlockSubmission> {
        Ok(self._set_submission_completed(fuel_block_hash).await?)
    }

    async fn insert_state(
        &self,
        state: StateSubmission,
        fragments: Vec<StateFragment>,
    ) -> ports::storage::Result<()> {
        Ok(self._insert_state(state, fragments).await?)
    }

    async fn get_unsubmitted_fragments(&self) -> ports::storage::Result<Vec<StateFragment>> {
        Ok(self._get_unsubmitted_fragments().await?)
    }

    async fn record_pending_tx(
        &self,
        tx_hash: [u8; 32],
        fragment_ids: Vec<StateFragmentId>,
    ) -> ports::storage::Result<()> {
        Ok(self._record_pending_tx(tx_hash, fragment_ids).await?)
    }

    async fn has_pending_txs(&self) -> ports::storage::Result<bool> {
        Ok(self._has_pending_txs().await?)
    }

    async fn state_submission_w_latest_block(
        &self,
    ) -> ports::storage::Result<Option<StateSubmission>> {
        Ok(self._state_submission_w_latest_block().await?)
    }
}

#[cfg(test)]
mod tests {
    use ports::{
        storage::{Error, Storage},
        types::BlockSubmission,
    };
    use rand::{thread_rng, Rng};
    use storage as _;

    use crate::PostgresProcess;

    fn random_non_zero_height() -> u32 {
        let mut rng = thread_rng();
        rng.gen_range(1..u32::MAX)
    }

    #[tokio::test]
    async fn can_insert_and_find_latest_block() {
        // given
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();
        let latest_height = random_non_zero_height();

        let latest_submission = given_incomplete_submission(latest_height);
        db.insert(latest_submission.clone()).await.unwrap();

        let older_submission = given_incomplete_submission(latest_height - 1);
        db.insert(older_submission).await.unwrap();

        // when
        let actual = db.submission_w_latest_block().await.unwrap().unwrap();

        // then
        assert_eq!(actual, latest_submission);
    }

    #[tokio::test]
    async fn can_update_completion_status() {
        // given
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();

        let height = random_non_zero_height();
        let submission = given_incomplete_submission(height);
        let block_hash = submission.block_hash;
        db.insert(submission).await.unwrap();

        // when
        let submission = db.set_submission_completed(block_hash).await.unwrap();

        // then
        assert!(submission.completed);
    }

    #[tokio::test]
    async fn updating_a_missing_submission_causes_an_error() {
        // given
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();

        let height = random_non_zero_height();
        let submission = given_incomplete_submission(height);
        let block_hash = submission.block_hash;

        // when
        let result = db.set_submission_completed(block_hash).await;

        // then
        let Err(Error::Database(msg)) = result else {
            panic!("should be storage error");
        };

        let block_hash = hex::encode(block_hash);
        assert_eq!(msg, format!("Cannot set submission to completed! Submission of block: `{block_hash}` not found in DB."));
    }

    fn given_incomplete_submission(fuel_block_height: u32) -> BlockSubmission {
        let mut submission = rand::thread_rng().gen::<BlockSubmission>();
        submission.block_height = fuel_block_height;

        submission
    }
}
