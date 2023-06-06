use crate::{adapters::storage::Storage, common::EthTxStatus, errors::Error};
use ethers::types::H256;
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};
use tokio::{sync::Mutex, task};

use crate::errors::Result;

use super::EthTxSubmission;

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
                    fuel_block_height    INTEGER PRIMARY KEY NOT NULL,
                    status TEXT NOT NULL,
                    tx_hash BLOB NOT NULL
                )"#,
            (), // empty list of parameters.
        )?;

        Ok(Arc::new(Mutex::new(connection)))
    }
}

#[async_trait::async_trait]
impl Storage for SqliteDb {
    async fn insert(&self, submission: EthTxSubmission) -> Result<()> {
        let fuel_block_height = submission.fuel_block_height;
        let status = submission.status.to_string();
        let tx_hash = submission.tx_hash.to_fixed_bytes();

        let connection = Arc::clone(&self.connection);
        task::spawn_blocking(move || async move {
            let connection = connection.lock().await;
            let query = "INSERT INTO eth_tx_submission (fuel_block_height, status, tx_hash) VALUES (?1, ?2, ?3)";
            connection.execute( query, (fuel_block_height, status, tx_hash))
        }).await.unwrap().await?;

        Ok(())
    }

    async fn update(&self, submission: EthTxSubmission) -> Result<()> {
        let fuel_block_height = submission.fuel_block_height;
        let status = submission.status.to_string();
        let tx_hash = submission.tx_hash.to_fixed_bytes();

        let connection = Arc::clone(&self.connection);
        task::spawn_blocking(move || async move {
            let connection = connection.lock().await;
            let query = "UPDATE eth_tx_submission SET status = (?1), tx_hash = (?2) WHERE fuel_block_height = (?3)";
            connection.execute( query, (&status, &tx_hash, &fuel_block_height))
        }).await.unwrap().await?;

        Ok(())
    }

    async fn submission_w_latest_block(&self) -> Result<Option<EthTxSubmission>> {
        let connection = Arc::clone(&self.connection);
        let Some((fuel_block_height, status, tx_hash)) = task::spawn_blocking(move || async move {
            let connection = connection.lock().await;

            let mut statement = connection.prepare(
                r#"SELECT * FROM eth_tx_submission ORDER BY fuel_block_height DESC LIMIT 1"#,
            )?;

            let result = statement
                .query_map([], |row| {
                    let fuel_block_height = row.get(0)?;
                    let status: String = row.get(1)?;
                    let tx_hash: [u8; 32] = row.get(2)?;

                    Ok((fuel_block_height, status, tx_hash))
                })?
                .next()
                .transpose();

            result
        })
        .await
        .unwrap()
        .await? else {
            return Ok(None);
        };

        let status =
            EthTxStatus::from_str(&status).map_err(|err| Error::StorageError(err.to_string()))?;

        let tx_hash = H256::from(tx_hash);

        Ok(Some(EthTxSubmission {
            fuel_block_height,
            status,
            tx_hash,
        }))
    }
}

#[cfg(test)]
mod tests {
    use ethers::types::H256;
    use rusqlite::Connection;

    use crate::{adapters::storage::EthTxSubmission, common::EthTxStatus};

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn can_insert_and_find_latest_block() {
        // given
        let storage = SqliteDb::temporary().await.unwrap();
        let latest_submission = given_pending_submission(10);
        storage.insert(latest_submission.clone()).await.unwrap();

        let older_submission = given_pending_submission(9);
        storage.insert(older_submission).await.unwrap();

        // when
        let actual = storage.submission_w_latest_block().await.unwrap().unwrap();

        // then
        assert_eq!(actual, latest_submission);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn can_update_block() {
        // given
        let storage = SqliteDb::temporary().await.unwrap();

        let mut latest_submission = given_pending_submission(10);
        storage.insert(latest_submission.clone()).await.unwrap();

        latest_submission.status = EthTxStatus::Committed;
        storage.update(latest_submission.clone()).await.unwrap();

        // when
        let actual = storage.submission_w_latest_block().await.unwrap().unwrap();

        // then
        assert_eq!(actual, latest_submission);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn correctly_gets_submission_w_latest_block() {
        let db = SqliteDb::temporary().await.unwrap();

        for current_height in 0..=1024 {
            let current_entry = given_pending_submission(current_height);
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

    fn given_pending_submission(fuel_block_height: u32) -> EthTxSubmission {
        EthTxSubmission {
            fuel_block_height,
            status: EthTxStatus::Pending,
            tx_hash: H256::default(),
        }
    }

    #[test]
    fn sqlite_test() {
        let conn = Connection::open_in_memory().unwrap();

        conn.execute(
            "CREATE TABLE eth_tx_submission (
            fuel_block_height    INTEGER PRIMARY KEY NOT NULL,
            status TEXT NOT NULL,
            tx_hash BLOB NOT NULL
        )",
            (), // empty list of parameters.
        )
        .unwrap();

        let me = EthTxSubmission {
            fuel_block_height: 10,
            status: EthTxStatus::Pending,
            tx_hash: H256::default(),
        };
        conn.execute(
            "INSERT INTO eth_tx_submission (fuel_block_height, status, tx_hash) VALUES (?1, ?2, ?3)",
            (&me.fuel_block_height, &me.status.to_string(), &me.tx_hash.to_fixed_bytes())
        )
        .unwrap();
        //
        // let mut stmt = conn.prepare("SELECT id, name, data FROM person").unwrap();
        // let person_iter = stmt
        //     .query_map([], |row| {
        //         Ok(Person {
        //             id: row.get(0)?,
        //             name: row.get(1)?,
        //             data: row.get(2)?,
        //         })
        //     })
        //     .unwrap();
        //
        // for person in person_iter {
        //     println!("Found person {:?}", person.unwrap());
        // }
    }
}
