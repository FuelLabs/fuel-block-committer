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
    types::{
        BlockSubmission, BlockSubmissionTx, StateFragment, StateSubmission, SubmissionTx,
        TransactionState,
    },
};
pub use postgres::{DbConfig, Postgres};

#[async_trait::async_trait]
impl Storage for Postgres {
    async fn record_block_submission(
        &self,
        submission_tx: BlockSubmissionTx,
        submission: BlockSubmission,
    ) -> Result<u32> {
        Ok(self
            ._record_block_submission(submission_tx, submission)
            .await?)
    }

    async fn get_pending_block_submission_txs(
        &self,
        submission_id: u32,
    ) -> Result<Vec<BlockSubmissionTx>> {
        Ok(self
            ._get_pending_block_submission_txs(submission_id)
            .await?)
    }

    async fn update_block_submission_tx(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<BlockSubmission> {
        Ok(self._update_block_submission_tx(hash, state).await?)
    }

    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>> {
        Ok(self._submission_w_latest_block().await?)
    }

    async fn insert_state_submission(
        &self,
        submission: StateSubmission,
        fragments: Vec<StateFragment>,
    ) -> Result<()> {
        Ok(self._insert_state_submission(submission, fragments).await?)
    }

    async fn get_unsubmitted_fragments(&self) -> Result<Vec<StateFragment>> {
        Ok(self._get_unsubmitted_fragments().await?)
    }

    async fn record_state_submission(
        &self,
        tx_hash: [u8; 32],
        fragment_ids: Vec<u32>,
    ) -> Result<()> {
        Ok(self._record_state_submission(tx_hash, fragment_ids).await?)
    }

    async fn get_pending_txs(&self) -> Result<Vec<SubmissionTx>> {
        Ok(self._get_pending_txs().await?)
    }

    async fn has_pending_state_submission(&self) -> Result<bool> {
        Ok(self._has_pending_fragments().await?)
    }

    async fn state_submission_w_latest_block(&self) -> Result<Option<StateSubmission>> {
        Ok(self._state_submission_w_latest_block().await?)
    }

    async fn update_submission_tx_state(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<()> {
        Ok(self._update_submission_tx_state(hash, state).await?)
    }
}

#[cfg(test)]
mod tests {
    use ports::{
        storage::{Error, Result, Storage},
        types::{
            BlockSubmission, BlockSubmissionTx, StateFragment, StateSubmission, TransactionState,
        },
    };
    use rand::{thread_rng, Rng};
    use storage as _;

    use crate::PostgresProcess;

    fn random_non_zero_height() -> u32 {
        let mut rng = thread_rng();
        rng.gen_range(1..u32::MAX)
    }

    #[tokio::test]
    async fn can_record_and_find_latest_block() {
        // given
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();
        let latest_height = random_non_zero_height();

        let latest_submission = given_incomplete_submission(latest_height);
        let submission_tx = given_pending_tx(0);
        let submission_id = db
            .record_block_submission(submission_tx, latest_submission.clone())
            .await
            .unwrap();

        let older_submission = given_incomplete_submission(latest_height - 1);
        let submission_tx = given_pending_tx(1);
        db.record_block_submission(submission_tx, older_submission)
            .await
            .unwrap();

        // when
        let actual = db.submission_w_latest_block().await.unwrap().unwrap();

        // then
        let expected = BlockSubmission {
            id: Some(submission_id),
            ..latest_submission
        };
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn can_record_and_find_pending_tx() {
        // given
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();
        let latest_height = random_non_zero_height();

        let latest_submission = given_incomplete_submission(latest_height);
        let submission_tx = given_pending_tx(0);

        let submission_id = db
            .record_block_submission(submission_tx.clone(), latest_submission.clone())
            .await
            .unwrap();

        // when
        let actual = db
            .get_pending_block_submission_txs(submission_id)
            .await
            .unwrap()
            .pop()
            .expect("pending tx to exist");

        // then
        let expected = BlockSubmissionTx {
            id: actual.id,
            created_at: actual.created_at,
            submission_id: Some(submission_id),
            ..submission_tx
        };
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn can_update_completion_status() {
        // given
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();

        let height = random_non_zero_height();
        let submission = given_incomplete_submission(height);
        let submission_tx = given_pending_tx(0);

        let submission_id = db
            .record_block_submission(submission_tx, submission)
            .await
            .unwrap();

        // when
        let submission_tx = db
            ._get_pending_block_submission_txs(submission_id)
            .await
            .unwrap()
            .pop()
            .expect("pending tx to exist");
        db.update_block_submission_tx(submission_tx.hash, TransactionState::Finalized)
            .await
            .unwrap();

        // then
        let pending_txs = db
            ._get_pending_block_submission_txs(submission_id)
            .await
            .unwrap();
        assert!(pending_txs.is_empty());

        let submission = db
            .submission_w_latest_block()
            .await
            .unwrap()
            .expect("submission to exist");
        assert!(submission.completed);
    }

    #[tokio::test]
    async fn updating_a_missing_submission_tx_causes_an_error() {
        // given
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();

        let submission_tx = given_pending_tx(0);

        // when
        let result = db
            .update_block_submission_tx(submission_tx.hash, TransactionState::Finalized)
            .await;

        // then
        let Err(Error::Database(msg)) = result else {
            panic!("should be storage error");
        };

        let tx_hash = hex::encode(submission_tx.hash);
        assert_eq!(
            msg,
            format!("Cannot update tx state! Tx with hash: `{tx_hash}` not found in DB.")
        );
    }

    fn given_incomplete_submission(fuel_block_height: u32) -> BlockSubmission {
        let mut submission = rand::thread_rng().gen::<BlockSubmission>();
        submission.block_height = fuel_block_height;

        submission
    }

    fn given_pending_tx(nonce: u32) -> BlockSubmissionTx {
        BlockSubmissionTx {
            hash: [nonce as u8; 32],
            nonce,
            state: TransactionState::Pending,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn insert_state_submission() -> Result<()> {
        // given
        let process = PostgresProcess::shared().await?;
        let db = process.create_random_db().await?;

        let (state, fragments) = given_state_and_fragments();

        // when
        db.insert_state_submission(state, fragments.clone()).await?;

        // then
        let db_fragments = db.get_unsubmitted_fragments().await?;

        assert_eq!(db_fragments.len(), fragments.len());

        Ok(())
    }

    #[tokio::test]
    async fn record_state_submission() -> Result<()> {
        // given
        let process = PostgresProcess::shared().await?;
        let db = process.create_random_db().await?;

        let (state, fragments) = given_state_and_fragments();
        db.insert_state_submission(state, fragments.clone()).await?;
        let tx_hash = [1; 32];
        let fragment_ids = vec![1];

        // when
        db.record_state_submission(tx_hash, fragment_ids).await?;

        // then
        let has_pending_tx = db.has_pending_state_submission().await?;
        let pending_tx = db.get_pending_txs().await?;

        assert!(has_pending_tx);

        assert_eq!(pending_tx.len(), 1);
        assert_eq!(pending_tx[0].hash, tx_hash);
        assert_eq!(pending_tx[0].state, TransactionState::Pending);

        Ok(())
    }

    #[tokio::test]
    async fn update_submission_tx_state() -> Result<()> {
        // given
        let process = PostgresProcess::shared().await?;
        let db = process.create_random_db().await?;

        let (state, fragments) = given_state_and_fragments();
        db.insert_state_submission(state, fragments.clone()).await?;
        let tx_hash = [1; 32];
        let fragment_ids = vec![1];
        db.record_state_submission(tx_hash, fragment_ids).await?;

        // when
        db.update_submission_tx_state(tx_hash, TransactionState::Finalized)
            .await?;

        // then
        let has_pending_tx = db.has_pending_state_submission().await?;
        let pending_tx = db.get_pending_txs().await?;

        assert!(!has_pending_tx);
        assert!(pending_tx.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn unsbumitted_fragments_are_not_in_pending_or_finalized_tx() -> Result<()> {
        // given
        let process = PostgresProcess::shared().await?;
        let db = process.create_random_db().await?;

        let (state, fragments) = given_state_and_fragments();
        db.insert_state_submission(state, fragments.clone()).await?;

        // when
        // tx failed
        let tx_hash = [1; 32];
        let fragment_ids = vec![1, 2];
        db.record_state_submission(tx_hash, fragment_ids).await?;
        db.update_submission_tx_state(tx_hash, TransactionState::Failed)
            .await?;

        // tx is finalized
        let tx_hash = [2; 32];
        let fragment_ids = vec![2];
        db.record_state_submission(tx_hash, fragment_ids).await?;
        db.update_submission_tx_state(tx_hash, TransactionState::Finalized)
            .await?;

        // tx is pending
        let tx_hash = [3; 32];
        let fragment_ids = vec![3];
        db.record_state_submission(tx_hash, fragment_ids).await?;

        // then
        let db_fragments = db.get_unsubmitted_fragments().await?;

        let db_fragment_id: Vec<_> = db_fragments.iter().map(|f| f.id.expect("has id")).collect();

        // unsubmitted fragments are not associated to any finalized or pending tx
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
