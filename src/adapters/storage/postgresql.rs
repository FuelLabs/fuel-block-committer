use std::time::Duration;

use super::{BlockSubmission, Error, Storage};
use crate::adapters::storage::Result;

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
    pub database: String,
    /// The maximum number of connections allowed in the connection pool.
    pub max_connections: u32,
}

impl Postgres {
    pub async fn connect(opt: &DbConfig) -> Result<Self> {
        let options = PgConnectOptions::new()
            .username(&opt.username)
            .password(&opt.password)
            .database(&opt.database)
            .host(&opt.host)
            .port(opt.port);

        let connection_pool = PgPoolOptions::new()
            .max_connections(opt.max_connections)
            .acquire_timeout(Duration::from_secs(2))
            .connect_with(options)
            .await?;

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

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

#[async_trait::async_trait]
impl Storage for Postgres {
    async fn insert(&self, submission: BlockSubmission) -> Result<()> {
        let row = tables::EthFuelBlockSubmission::from(submission);
        sqlx::query!(
            "INSERT INTO eth_fuel_block_submission (fuel_block_hash, fuel_block_height, completed, submittal_height) VALUES ($1, $2, $3, $4)",
            row.fuel_block_hash,
            row.fuel_block_height,
            row.completed,
            row.submittal_height
        ).execute(&self.connection_pool).await?;
        Ok(())
    }

    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>> {
        sqlx::query_as!(
            tables::EthFuelBlockSubmission,
            "SELECT * FROM eth_fuel_block_submission ORDER BY fuel_block_height DESC LIMIT 1"
        )
        .fetch_optional(&self.connection_pool)
        .await?
        .map(BlockSubmission::try_from)
        .transpose()
    }

    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission> {
        let updated_row = sqlx::query_as!(
            tables::EthFuelBlockSubmission,
            "UPDATE eth_fuel_block_submission SET completed = true WHERE fuel_block_hash = $1 RETURNING *",
            fuel_block_hash.as_slice(),
        ).fetch_optional(&self.connection_pool).await?;

        if let Some(row) = updated_row {
            Ok(row.try_into()?)
        } else {
            let hash = hex::encode(fuel_block_hash);
            Err(Error::Database(format!("Cannot set submission to completed! Submission of block: `{hash}` not found in DB.")))
        }
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
