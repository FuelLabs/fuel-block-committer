#![deny(unused_crate_dependencies)]
mod tables;
#[cfg(feature = "test-helpers")]
mod test_instance;
#[cfg(feature = "test-helpers")]
pub use test_instance::*;

mod postgres;
pub use postgres::*;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Database Error {0}")]
    Database(String),
    #[error("Could not convert to/from domain/db type {0}")]
    Conversion(String),
    #[error("{0}")]
    Other(String),
}

impl From<Error> for ports::storage::Error {
    fn from(value: Error) -> Self {
        Self::new(value.to_string())
    }
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Self::Database(e.to_string())
    }
}

impl From<sqlx::migrate::MigrateError> for Error {
    fn from(e: sqlx::migrate::MigrateError) -> Self {
        Self::Database(e.to_string())
    }
}

use ports::types::BlockSubmission;

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
}

#[cfg(test)]
mod tests {
    use ports::types::BlockSubmission;
    use rand::{thread_rng, Rng};
    use storage as _;

    use crate::{Error, PostgresProcess};

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
        db._insert(latest_submission.clone()).await.unwrap();

        let older_submission = given_incomplete_submission(latest_height - 1);
        db._insert(older_submission).await.unwrap();

        // when
        let actual = db._submission_w_latest_block().await.unwrap().unwrap();

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
        let block_hash = submission.block.hash;
        db._insert(submission).await.unwrap();

        // when
        let submission = db._set_submission_completed(block_hash).await.unwrap();

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
        let block_hash = submission.block.hash;

        // when
        let result = db._set_submission_completed(block_hash).await;

        // then
        let Err(Error::Database(msg)) = result else {
            panic!("should be storage error");
        };

        let block_hash = hex::encode(block_hash);
        assert_eq!(msg, format!("Cannot set submission to completed! Submission of block: `{block_hash}` not found in DB."));
    }

    fn given_incomplete_submission(fuel_block_height: u32) -> BlockSubmission {
        let mut submission = rand::thread_rng().gen::<BlockSubmission>();
        submission.block.height = fuel_block_height;

        submission
    }
}
