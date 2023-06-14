use std::{path::PathBuf, sync::Arc};

use fuels::tx::Bytes32;
use rusqlite::{Connection, Row};
use tokio::{sync::Mutex, task};

use crate::{
    adapters::storage::{BlockSubmission, Storage},
    errors::{Error, Result},
};

#[derive(Clone)]
pub struct SqliteDb {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteDb {
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        task::spawn_blocking(|| async {
            let connection = Connection::open(path)?;
            Ok(Self {
                connection: Self::initialize(connection)?,
            })
        })
        .await
        .unwrap()
        .await
    }

    pub async fn temporary() -> Result<Self> {
        task::spawn_blocking(|| async {
            let connection = Connection::open_in_memory()?;
            Ok(Self {
                connection: Self::initialize(connection)?,
            })
        })
        .await
        .unwrap()
        .await
    }

    fn initialize(connection: Connection) -> Result<Arc<Mutex<Connection>>> {
        connection.execute(
            r#"CREATE TABLE IF NOT EXISTS eth_tx_submission (
                    fuel_block_hash     BLOB PRIMARY KEY NOT NULL,
                    fuel_block_height   INTEGER NOT NULL UNIQUE,
                    completed           INTEGER NOT NULL,
                    submitted_at_height BLOB NOT NULL
                )"#,
            (), // empty list of parameters.
        )?;

        Ok(Arc::new(Mutex::new(connection)))
    }

    async fn run_blocking<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&Connection) -> T + Send + 'static,
        T: Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        task::spawn_blocking(move || async move {
            let connection = connection.lock().await;
            f(&connection)
        })
        .await
        .unwrap()
        .await
    }

    fn decode_submission(row: &Row) -> std::result::Result<BlockSubmission, rusqlite::Error> {
        let fuel_block_hash: [u8; 32] = row.get(0)?;
        let fuel_block_height: u32 = row.get(1)?;
        let completed: bool = row.get(2)?;

        let submitted_at_height = {
            let le_bytes: [u8; 8] = row.get(3)?;
            u64::from_le_bytes(le_bytes)
        };

        Ok(BlockSubmission {
            fuel_block_hash: fuel_block_hash.into(),
            fuel_block_height,
            completed,
            submitted_at_height: submitted_at_height.into(),
        })
    }
}

#[async_trait::async_trait]
impl Storage for SqliteDb {
    async fn insert(&self, submission: BlockSubmission) -> Result<()> {
        let BlockSubmission {
            fuel_block_hash,
            fuel_block_height,
            completed,
            submitted_at_height,
        } = submission;
        let submitted_at_height = submitted_at_height.as_u64().to_le_bytes();

        self.run_blocking(move |connection| {
            let query = "INSERT INTO eth_tx_submission (fuel_block_hash, fuel_block_height, completed, submitted_at_height) VALUES (?1, ?2, ?3, ?4)";
            connection.execute( query, (*fuel_block_hash, fuel_block_height, completed, submitted_at_height))
        }).await?;

        Ok(())
    }

    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>> {
        Ok(self
            .run_blocking(move |connection| {
                let mut statement = connection.prepare(
                    r#"SELECT * FROM eth_tx_submission ORDER BY fuel_block_height DESC LIMIT 1"#,
                )?;

                let mut submission = statement.query_map([], Self::decode_submission)?;

                submission.next().transpose()
            })
            .await?)
    }

    async fn set_submission_completed(&self, fuel_block_hash: Bytes32) -> Result<BlockSubmission> {
        self.run_blocking(move |connection| {
            let query = "UPDATE eth_tx_submission SET completed = 1 WHERE fuel_block_hash = (?1)";
            let rows_updated = connection.execute(query, (*fuel_block_hash,))?;

            if rows_updated == 0 {
                return Err(Error::StorageError(format!(
                    "Block: `{fuel_block_hash}` in DB"
                )));
            }

            let submission = connection.query_row(
                r#"SELECT * FROM eth_tx_submission WHERE fuel_block_hash = (?1)"#,
                (*fuel_block_hash,),
                Self::decode_submission,
            )?;

            Result::Ok(submission)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::storage::BlockSubmission;

    #[tokio::test]
    async fn can_insert_and_find_latest_block() {
        // given
        let storage = SqliteDb::temporary().await.unwrap();
        let latest_submission = given_incomplete_submission(10);
        storage.insert(latest_submission.clone()).await.unwrap();

        let older_submission = given_incomplete_submission(9);
        storage.insert(older_submission).await.unwrap();

        // when
        let actual = storage.submission_w_latest_block().await.unwrap().unwrap();

        // then
        assert_eq!(actual, latest_submission);
    }

    #[tokio::test]
    async fn correctly_gets_submission_w_latest_block() {
        let db = SqliteDb::temporary().await.unwrap();

        for current_height in 0..=1024 {
            let current_entry = given_incomplete_submission(current_height);
            db.insert(current_entry).await.unwrap();

            let highest_block_height = db
                .submission_w_latest_block()
                .await
                .unwrap()
                .unwrap()
                .fuel_block_height;

            assert_eq!(highest_block_height, current_height);
        }
    }

    #[tokio::test]
    async fn can_update_completion_status() {
        // given
        let db = SqliteDb::temporary().await.unwrap();

        let submission = given_incomplete_submission(10);
        let block_hash = submission.fuel_block_hash;
        db.insert(submission).await.unwrap();

        // when
        let submission = db.set_submission_completed(block_hash).await.unwrap();

        // then
        assert!(submission.completed);
    }

    #[tokio::test]
    async fn signals_that_update_didnt_happen_due_to_missing_submission() {
        // given
        let db = SqliteDb::temporary().await.unwrap();

        let submission = given_incomplete_submission(10);
        let block_hash = submission.fuel_block_hash;

        // when
        let result = db.set_submission_completed(block_hash).await;

        // then
        let Err(Error::StorageError(msg)) = result else {
            panic!("should be storage error");
        };

        assert_eq!(msg, format!("Block: `{block_hash}` in DB"))
    }

    fn given_incomplete_submission(fuel_block_height: u32) -> BlockSubmission {
        BlockSubmission {
            fuel_block_height,
            ..BlockSubmission::random()
        }
    }
}
