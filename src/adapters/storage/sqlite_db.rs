use crate::{adapters::storage::Storage, common::EthTxStatus};
use ethers::types::H256;
use rusqlite::Connection;
use std::{path::Path, str::FromStr, sync::Arc};
use tokio::sync::Mutex;

use crate::errors::Result;

use super::EthTxSubmission;

#[derive(Clone)]
pub struct SqliteDb {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteDb {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path).unwrap();
        Ok(Self {
            connection: Self::initialize(connection)?,
        })
    }
    pub fn temporary() -> Result<Self> {
        let connection = Connection::open_in_memory().unwrap();
        Ok(Self {
            connection: Self::initialize(connection)?,
        })
    }

    fn initialize(connection: Connection) -> Result<Arc<Mutex<Connection>>> {
        connection
            .execute(
                r#"CREATE TABLE IF NOT EXISTS eth_tx_submission (
                    fuel_block_height    INTEGER PRIMARY KEY NOT NULL,
                    status TEXT NOT NULL,
                    tx_hash BLOB NOT NULL
                )"#,
                (), // empty list of parameters.
            )
            .unwrap();

        Ok(Arc::new(Mutex::new(connection)))
    }
}

#[async_trait::async_trait]
impl Storage for SqliteDb {
    async fn insert(&self, submission: EthTxSubmission) -> Result<()> {
        let fuel_block_height = submission.fuel_block_height;
        let status = submission.status.to_string();
        let tx_hash = submission.tx_hash.to_fixed_bytes();

        self.connection.lock().await.execute(
            "INSERT INTO eth_tx_submission (fuel_block_height, status, tx_hash) VALUES (?1, ?2, ?3)",
            (&fuel_block_height, &status, &tx_hash)
        )
        .unwrap();

        Ok(())
    }

    async fn update(&self, submission: EthTxSubmission) -> Result<()> {
        let fuel_block_height = submission.fuel_block_height;
        let status = submission.status.to_string();
        let tx_hash = submission.tx_hash.to_fixed_bytes();

        self.connection.lock().await.execute(
            r#"UPDATE eth_tx_submission SET status = (?1), tx_hash = (?2) WHERE fuel_block_height = (?3)"#,
            (&status, &tx_hash, &fuel_block_height)
        )
        .unwrap();

        Ok(())
    }

    async fn submission_w_latest_block(&self) -> Result<Option<EthTxSubmission>> {
        let connection = self.connection.lock().await;

        let mut statement = connection
            .prepare(r#"select * from eth_tx_submission ORDER BY fuel_block_height DESC LIMIT 1"#)
            .unwrap();

        let value = statement
            .query_map([], |row| {
                let fuel_block_height = row.get(0)?;
                let str: String = row.get(1)?;
                let status = EthTxStatus::from_str(&str).unwrap();
                let bytes: [u8; 32] = row.get(2)?;
                let tx_hash = H256::from(bytes);

                Ok(EthTxSubmission {
                    fuel_block_height,
                    status,
                    tx_hash,
                })
            })
            .unwrap()
            .next()
            .map(|r| r.unwrap());

        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use ethers::types::H256;
    use rusqlite::Connection;

    use crate::{adapters::storage::EthTxSubmission, common::EthTxStatus};

    use super::*;

    #[tokio::test]
    async fn can_insert_and_find_latest_block() {
        // given
        let storage = SqliteDb::temporary().unwrap();
        let latest_submission = given_pending_submission(10);
        storage.insert(latest_submission.clone()).await.unwrap();

        let older_submission = given_pending_submission(9);
        storage.insert(older_submission).await.unwrap();

        // when
        let actual = storage.submission_w_latest_block().await.unwrap().unwrap();

        // then
        assert_eq!(actual, latest_submission);
    }

    #[tokio::test]
    async fn can_update_block() {
        // given
        let storage = SqliteDb::temporary().unwrap();

        let mut latest_submission = given_pending_submission(10);
        storage.insert(latest_submission.clone()).await.unwrap();

        latest_submission.status = EthTxStatus::Committed;
        storage.update(latest_submission.clone()).await.unwrap();

        // when
        let actual = storage.submission_w_latest_block().await.unwrap().unwrap();

        // then
        assert_eq!(actual, latest_submission);
    }

    #[tokio::test]
    async fn correctly_gets_submission_w_latest_block() {
        let db = SqliteDb::temporary().unwrap();

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
