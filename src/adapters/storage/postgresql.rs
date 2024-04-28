use std::sync::Arc;

use super::{BlockSubmission, Error, Storage};
use crate::adapters::storage::Result;

#[derive(Clone)]
pub struct PostgresDb {
    connection_pool: sqlx::Pool<sqlx::Postgres>,
}

impl PostgresDb {
    pub async fn tx(&self) -> Result<Transaction> {
        let transaction = self.connection_pool.begin().await?;
        Ok(Transaction {
            transaction: Mutex::new(transaction),
        })
    }
}

pub struct Transaction {
    transaction: Mutex<sqlx::Transaction<'static, sqlx::Postgres>>,
}

impl Transaction {
    pub async fn commit(self) -> Result<()> {
        self.transaction.into_inner().commit().await?;
        Ok(())
    }

    pub async fn rollback(self) -> Result<()> {
        self.transaction.into_inner().rollback().await?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionOptions {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub db: String,
    pub connections: u32,
}

impl ConnectionOptions {
    async fn establish_pool(&self) -> sqlx::Result<sqlx::Pool<sqlx::Postgres>> {
        let options = PgConnectOptions::new()
            .username(&self.username)
            .password(&self.password)
            .database(&self.db)
            .host(&self.host)
            .port(self.port);

        PgPoolOptions::new()
            .max_connections(self.connections)
            .connect_with(options)
            .await
    }
}

impl PostgresDb {
    pub async fn connect(opt: &ConnectionOptions) -> Result<Self> {
        let connection_pool = opt.establish_pool().await?;

        Ok(Self { connection_pool })
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!().run(&self.connection_pool).await?;
        Ok(())
    }
}

mod tables {
    use crate::adapters::{
        fuel_adapter::FuelBlock,
        storage::{BlockSubmission, Error},
    };

    #[derive(sqlx::FromRow)]
    pub struct EthFuelBlockSubmission {
        pub fuel_block_hash: Vec<u8>,
        pub fuel_block_height: i64,
        pub completed: bool,
        pub submittal_height: i64,
    }

    impl TryFrom<EthFuelBlockSubmission> for BlockSubmission {
        type Error = Error;

        fn try_from(value: EthFuelBlockSubmission) -> Result<Self, Self::Error> {
            let block_hash = value.fuel_block_hash.as_slice();
            macro_rules! bail {
                ($msg: literal, $($args: expr),*) => {
                    return Err(Error::Conversion(format!($msg, $($args),*)));
                };
            }
            let Ok(hash) = block_hash.try_into() else {
                bail!("Expected 32 bytes for `fuel_block_hash`, but got: {block_hash:?} from db",);
            };

            let Ok(height) = value.fuel_block_height.try_into() else {
                bail!(
                    "`fuel_block_height` as read from the db cannot fit in a `u32` as expected. Got: {:?} from db",
                    value.fuel_block_height

                );
            };

            let Ok(submittal_height) = value.submittal_height.try_into() else {
                bail!("`submittal_height` as read from the db cannot fit in a `u64` as expected. Got: {} from db", value.submittal_height);
            };

            Ok(Self {
                block: FuelBlock { hash, height },
                completed: value.completed,
                submittal_height,
            })
        }
    }

    impl From<BlockSubmission> for EthFuelBlockSubmission {
        fn from(value: BlockSubmission) -> Self {
            Self {
                fuel_block_hash: value.block.hash.to_vec(),
                fuel_block_height: i64::from(value.block.height),
                completed: value.completed,
                submittal_height: value.submittal_height.into(),
            }
        }
    }
}

#[cfg(test)]
pub use dockerized::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::Mutex;

#[async_trait::async_trait]
impl Storage for PostgresDb {
    async fn insert(&self, submission: BlockSubmission) -> Result<()> {
        let tx = self.tx().await?;

        tx.insert(submission).await?;

        tx.commit().await?;

        Ok(())
    }

    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>> {
        let tx = self.tx().await?;

        let response = tx.submission_w_latest_block().await?;

        tx.commit().await?;

        Ok(response)
    }

    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission> {
        let tx = self.tx().await?;

        let response = tx.set_submission_completed(fuel_block_hash).await?;

        tx.commit().await?;

        Ok(response)
    }
}

#[async_trait::async_trait]
impl Storage for Transaction {
    async fn insert(&self, submission: BlockSubmission) -> Result<()> {
        let row = tables::EthFuelBlockSubmission::from(submission);
        let mut tx = self.transaction.lock().await;
        sqlx::query!(
            "INSERT INTO eth_fuel_block_submission (fuel_block_hash, fuel_block_height, completed, submittal_height) VALUES ($1, $2, $3, $4)",
            row.fuel_block_hash,
            row.fuel_block_height,
            row.completed,
            row.submittal_height
        ).execute(tx.as_mut()).await?;
        Ok(())
    }

    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>> {
        let mut tx = self.transaction.lock().await;
        sqlx::query_as!(
            tables::EthFuelBlockSubmission,
            "SELECT * FROM eth_fuel_block_submission ORDER BY fuel_block_height DESC LIMIT 1"
        )
        .fetch_optional(tx.as_mut())
        .await?
        .map(BlockSubmission::try_from)
        .transpose()
    }

    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission> {
        let mut tx = self.transaction.lock().await;
        let updated_row = sqlx::query_as!(
            tables::EthFuelBlockSubmission,
            "UPDATE eth_fuel_block_submission SET completed = true WHERE fuel_block_hash = $1 RETURNING *",
            fuel_block_hash.as_slice(),
        ).fetch_optional(tx.as_mut()).await?;

        if let Some(row) = updated_row {
            Ok(row.try_into()?)
        } else {
            let hash = hex::encode(fuel_block_hash);
            Err(Error::Database(format!("Cannot set submission to completed! Submission of block: `{hash}` not found in DB.")))
        }
    }
}

#[cfg(test)]
mod dockerized {
    use std::sync::{Arc, Weak};

    use testcontainers::{core::WaitFor, runners::AsyncRunner, Image, RunnableImage};
    use tokio::sync::OnceCell;

    use super::{ConnectionOptions, PostgresDb};
    use crate::adapters::storage::Result;

    struct PostgresImage;

    impl Image for PostgresImage {
        type Args = ();

        fn name(&self) -> String {
            "postgres".to_owned()
        }

        fn tag(&self) -> String {
            "latest".to_owned()
        }

        fn ready_conditions(&self) -> Vec<WaitFor> {
            vec![WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            )]
        }

        fn expose_ports(&self) -> Vec<u16> {
            vec![5432]
        }
    }

    pub struct PostgresProcess {
        container: testcontainers::ContainerAsync<PostgresImage>,
        db: PostgresDb,
    }

    impl PostgresProcess {
        pub async fn shared() -> Result<Arc<Self>> {
            // If at some point no tests are running, the shared instance will be dropped. If
            // requested again, it will be recreated.
            // This is a workaround for the lack of a global setup/teardown for tests.
            static LOCK: tokio::sync::Mutex<Weak<PostgresProcess>> =
                tokio::sync::Mutex::const_new(Weak::new());
            let mut shared_process = LOCK.lock().await;

            let process = if let Some(running_process) = shared_process.upgrade() {
                running_process
            } else {
                let process = Arc::new(Self::start().await?);
                *shared_process = Arc::downgrade(&process);
                process
            };

            Ok(process)
        }

        pub async fn start() -> Result<Self> {
            let username = "username".to_string();
            let password = "password".to_string();
            let db = "test".to_string();

            let container = Self::start_docker_container(&username, &password, &db).await;

            let pool = setup_connection_pool(username, password, db, &container).await?;

            let db = PostgresDb {
                connection_pool: pool,
            };

            db.migrate().await?;

            Ok(Self { container, db })
        }

        pub fn db(&self) -> PostgresDb {
            self.db.clone()
        }

        pub async fn stop(&self) {
            self.container.stop().await;
        }

        async fn start_docker_container(
            username: &String,
            password: &String,
            db: &String,
        ) -> testcontainers::ContainerAsync<PostgresImage> {
            RunnableImage::from(PostgresImage)
                .with_env_var(("POSTGRES_USER", username))
                .with_env_var(("POSTGRES_PASSWORD", password))
                .with_env_var(("POSTGRES_DB", db))
                .start()
                .await
        }
    }

    async fn setup_connection_pool(
        username: String,
        password: String,
        db: String,
        container: &testcontainers::ContainerAsync<PostgresImage>,
    ) -> Result<sqlx::Pool<sqlx::Postgres>> {
        let config = ConnectionOptions {
            username,
            password,
            db,
            host: "localhost".to_string(),
            port: container.get_host_port_ipv4(5432).await,
            connections: 5,
        };

        Ok(config.establish_pool().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::storage::{BlockSubmission, Error};

    #[tokio::test]
    async fn can_insert_and_find_latest_block() {
        // given
        let process = PostgresProcess::shared().await.unwrap();
        let tx = process.db().tx().await.unwrap();

        let latest_submission = given_incomplete_submission(10);
        tx.insert(latest_submission.clone()).await.unwrap();

        let older_submission = given_incomplete_submission(9);
        tx.insert(older_submission).await.unwrap();

        // when
        let actual = tx.submission_w_latest_block().await.unwrap().unwrap();

        // then
        assert_eq!(actual, latest_submission);
    }

    #[tokio::test]
    async fn correctly_gets_submission_w_latest_block() {
        let process = PostgresProcess::shared().await.unwrap();
        let tx = process.db().tx().await.unwrap();

        for current_height in 0..=1024 {
            let current_entry = given_incomplete_submission(current_height);
            tx.insert(current_entry).await.unwrap();

            let highest_block_height = tx
                .submission_w_latest_block()
                .await
                .unwrap()
                .unwrap()
                .block
                .height;

            assert_eq!(highest_block_height, current_height);
        }
    }

    #[tokio::test]
    async fn can_update_completion_status() {
        // given
        let process = PostgresProcess::shared().await.unwrap();
        let tx = process.db().tx().await.unwrap();

        let submission = given_incomplete_submission(10);
        let block_hash = submission.block.hash;
        tx.insert(submission).await.unwrap();

        // when
        let submission = tx.set_submission_completed(block_hash).await.unwrap();

        // then
        assert!(submission.completed);
    }

    #[tokio::test]
    async fn updating_a_missing_submission_causes_an_error() {
        // given
        let process = PostgresProcess::shared().await.unwrap();
        let tx = process.db().tx().await.unwrap();

        let submission = given_incomplete_submission(10);
        let block_hash = submission.block.hash;

        // when
        let result = tx.set_submission_completed(block_hash).await;

        // then
        let Err(Error::Database(msg)) = result else {
            panic!("should be storage error");
        };

        let block_hash = hex::encode(block_hash);
        assert_eq!(msg, format!("Cannot set submission to completed! Submission of block: `{block_hash}` not found in DB."));
    }

    fn given_incomplete_submission(fuel_block_height: u32) -> BlockSubmission {
        let mut submission = BlockSubmission::random();
        submission.block.height = fuel_block_height;

        submission
    }
}
