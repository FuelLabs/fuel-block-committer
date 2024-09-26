#![deny(unused_crate_dependencies)]
mod tables;
#[cfg(feature = "test-helpers")]
mod test_instance;
#[cfg(feature = "test-helpers")]
pub use test_instance::*;

mod error;
mod postgres;
use ports::{
    storage::{Result, Storage},
    types::{BlockSubmission, StateFragment, StateSubmission, SubmissionTx, TransactionState},
};
pub use postgres::{DbConfig, Postgres};

#[async_trait::async_trait]
impl Storage for Postgres {
    async fn insert(&self, submission: BlockSubmission) -> Result<()> {
        self.insert_submission(submission).await
    }

    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>> {
        self.get_latest_submission().await
    }

    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission> {
        self.mark_submission_completed(fuel_block_hash).await
    }

    async fn insert_state_submission(
        &self,
        submission: StateSubmission,
        fragments: Vec<StateFragment>,
    ) -> Result<()> {
        self.insert_state_submission(submission, fragments).await
    }

    async fn get_unsubmitted_fragments(&self) -> Result<Vec<StateFragment>> {
        self.get_unsubmitted_fragments().await
    }

    async fn record_pending_tx(&self, tx_hash: [u8; 32], fragment_ids: Vec<u32>) -> Result<()> {
        self.record_pending_tx(tx_hash, fragment_ids).await
    }

    async fn get_pending_txs(&self) -> Result<Vec<SubmissionTx>> {
        self.get_pending_txs().await
    }

    async fn has_pending_txs(&self) -> Result<bool> {
        self.has_pending_txs().await
    }

    async fn state_submission_w_latest_block(&self) -> Result<Option<StateSubmission>> {
        self.get_latest_state_submission().await
    }

    async fn update_submission_tx_state(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<()> {
        self.update_submission_tx_state(hash, state).await
    }
}

#[cfg(test)]
mod tests {
    use ports::{
        storage::{Error, Result, Storage},
        types::{BlockSubmission, StateFragment, StateSubmission, TransactionState},
    };
    use rand::{thread_rng, Rng};
    use storage as _;

    use crate::PostgresProcess;

    fn random_non_zero_height() -> u32 {
        thread_rng().gen_range(1..u32::MAX)
    }

    #[tokio::test]
    async fn can_insert_and_find_latest_block() {
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();
        let latest_height = random_non_zero_height();

        let latest_submission = given_incomplete_submission(latest_height);
        db.insert(latest_submission.clone()).await.unwrap();

        let older_submission = given_incomplete_submission(latest_height - 1);
        db.insert(older_submission).await.unwrap();

        let actual = db.submission_w_latest_block().await.unwrap().unwrap();

        assert_eq!(actual, latest_submission);
    }

    #[tokio::test]
    async fn can_update_completion_status() {
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();

        let height = random_non_zero_height();
        let submission = given_incomplete_submission(height);
        let block_hash = submission.block_hash;
        db.insert(submission).await.unwrap();

        let submission = db.set_submission_completed(block_hash).await.unwrap();

        assert!(submission.completed);
    }

    #[tokio::test]
    async fn updating_a_missing_submission_causes_an_error() {
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();

        let height = random_non_zero_height();
        let submission = given_incomplete_submission(height);
        let block_hash = submission.block_hash;

        let result = db.set_submission_completed(block_hash).await;

        let Err(Error::Database(msg)) = result else {
            panic!("should be storage error");
        };

        let block_hash = hex::encode(block_hash);
        assert_eq!(msg, format!("Cannot set submission to completed! Submission of block: `{block_hash}` not found in DB."));
    }

    fn given_incomplete_submission(fuel_block_height: u32) -> BlockSubmission {
        let mut submission = thread_rng().gen::<BlockSubmission>();
        submission.block_height = fuel_block_height;
        submission
    }

    #[tokio::test]
    async fn insert_state_submission() -> Result<()> {
        let process = PostgresProcess::shared().await?;
        let db = process.create_random_db().await?;

        let (state, fragments) = given_state_and_fragments();

        db.insert_state_submission(state, fragments.clone()).await?;

        let db_fragments = db.get_unsubmitted_fragments().await?;

        assert_eq!(db_fragments.len(), fragments.len());

        Ok(())
    }

    #[tokio::test]
    async fn record_pending_tx() -> Result<()> {
        let process = PostgresProcess::shared().await?;
        let db = process.create_random_db().await?;

        let (state, fragments) = given_state_and_fragments();
        db.insert_state_submission(state, fragments.clone()).await?;
        let tx_hash = [1; 32];
        let fragment_ids = vec![1];

        db.record_pending_tx(tx_hash, fragment_ids).await?;

        let has_pending_tx = db.has_pending_txs().await?;
        let pending_tx = db.get_pending_txs().await?;

        assert!(has_pending_tx);
        assert_eq!(pending_tx.len(), 1);
        assert_eq!(pending_tx[0].hash, tx_hash);
        assert_eq!(pending_tx[0].state, TransactionState::Pending);

        Ok(())
    }

    #[tokio::test]
    async fn update_submission_tx_state() -> Result<()> {
        let process = PostgresProcess::shared().await?;
        let db = process.create_random_db().await?;

        let (state, fragments) = given_state_and_fragments();
        db.insert_state_submission(state, fragments.clone()).await?;
        let tx_hash = [1; 32];
        let fragment_ids = vec![1];
        db.record_pending_tx(tx_hash, fragment_ids).await?;

        db.update_submission_tx_state(tx_hash, TransactionState::Finalized).await?;

        let has_pending_tx = db.has_pending_txs().await?;
        let pending_tx = db.get_pending_txs().await?;

        assert!(!has_pending_tx);
        assert!(pending_tx.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn unsubmitted_fragments_are_not_in_pending_or_finalized_tx() -> Result<()> {
        let process = PostgresProcess::shared().await?;
        let db = process.create_random_db().await?;

        let (state, fragments) = given_state_and_fragments();
        db.insert_state_submission(state, fragments.clone()).await?;

        let tx_hash = [1; 32];
        let fragment_ids = vec![1, 2];
        db.record_pending_tx(tx_hash, fragment_ids).await?;
        db.update_submission_tx_state(tx_hash, TransactionState::Failed).await?;

        let tx_hash = [2; 32];
        let fragment_ids = vec![2];
        db.record_pending_tx(tx_hash, fragment_ids).await?;
        db.update_submission_tx_state(tx_hash, TransactionState::Finalized).await?;

        let tx_hash = [3; 32];
        let fragment_ids = vec![3];
        db.record_pending_tx(tx_hash, fragment_ids).await?;

        let db_fragments = db.get_unsubmitted_fragments().await?;
        let db_fragment_id: Vec<_> = db_fragments.iter().map(|f| f.id.expect("has id")).collect();

        assert_eq!(db_fragment_id, vec![1, 4, 5]);

        Ok(())
    }

    fn given_state_and_fragments() -> (StateSubmission, Vec<StateFragment>) {
        (
            StateSubmission {
                id: None,
                block_hash: [0u8; 32],
                block_height: 1,
            },
            vec![
                StateFragment {
                    id: None,
                    submission_id: None,
                    fragment_idx: 0,
                    data: vec![1, 2],
                    created_at: ports::types::Utc::now(),
                },
                StateFragment {
                    id: None,
                    submission_id: None,
                    fragment_idx: 1,
                    data: vec![3, 4],
                    created_at: ports::types::Utc::now(),
                },
                StateFragment {
                    id: None,
                    submission_id: None,
                    fragment_idx: 2,
                    data: vec![5, 6],
                    created_at: ports::types::Utc::now(),
                },
                StateFragment {
                    id: None,
                    submission_id: None,
                    fragment_idx: 3,
                    data: vec![7, 8],
                    created_at: ports::types::Utc::now(),
                },
                StateFragment {
                    id: None,
                    submission_id: None,
                    fragment_idx: 4,
                    data: vec![9, 10],
                    created_at: ports::types::Utc::now(),
                },
            ],
        )
    }
}
