use std::time::Duration;

use super::{BlockSubmission, Storage};
use crate::adapters::storage::Result;

mod queries;
mod tables;
#[cfg(test)]
mod test_instance;

use serde::Deserialize;
#[cfg(test)]
pub use test_instance::*;

#[derive(Clone)]
pub struct Postgres {
    connection_pool: sqlx::Pool<sqlx::Postgres>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DbConfig {
    /// The hostname or IP address of the PostgreSQL server.
    pub host: String,
    /// The port number on which the PostgreSQL server is listening.
    pub port: u16,
    /// The username used to authenticate with the PostgreSQL server.
    pub username: String,
    /// The password used to authenticate with the PostgreSQL server.
    pub password: String,
    /// The name of the database to connect to on the PostgreSQL server.
    pub db: String,
    /// The maximum number of connections allowed in the connection pool.
    pub max_connections: u32,
}

impl Postgres {
    pub async fn connect(opt: &DbConfig) -> Result<Self> {
        let connection_pool = opt.establish_pool().await?;

        Ok(Self { connection_pool })
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!().run(&self.connection_pool).await?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn execute(&self, query: &str) -> Result<()> {
        sqlx::query(query).execute(&self.connection_pool).await?;
        Ok(())
    }
}

impl DbConfig {
    async fn establish_pool(&self) -> sqlx::Result<sqlx::Pool<sqlx::Postgres>> {
        let options = PgConnectOptions::new()
            .username(&self.username)
            .password(&self.password)
            .database(&self.db)
            .host(&self.host)
            .port(self.port);

        PgPoolOptions::new()
            .max_connections(self.max_connections)
            .acquire_timeout(Duration::from_secs(2))
            .connect_with(options)
            .await
    }
}

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

#[async_trait::async_trait]
impl Storage for Postgres {
    async fn insert(&self, submission: BlockSubmission) -> Result<()> {
        queries::Queries::insert(&self.connection_pool, submission).await
    }

    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>> {
        queries::Queries::submission_w_latest_block(&self.connection_pool).await
    }

    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission> {
        queries::Queries::set_submission_completed(&self.connection_pool, fuel_block_hash).await
    }
}

#[cfg(test)]
mod tests {
    use rand::{thread_rng, Rng};

    use super::*;
    use crate::adapters::storage::{BlockSubmission, Error};

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
        let block_hash = submission.block.hash;
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
        let block_hash = submission.block.hash;

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
        submission.block.height = fuel_block_height;

        submission
    }
}
