pub mod in_memory_db;
pub mod sled_db;

use async_trait::async_trait;
use ethers::types::H256;
use serde::{Deserialize, Serialize};

use crate::{common::EthTxStatus, errors::Result};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EthTxSubmission {
    pub fuel_block_height: u32,
    pub status: EthTxStatus,
    pub tx_hash: H256,
}

#[async_trait]
pub trait Storage: Send + Sync {
    async fn insert(&self, submission: EthTxSubmission) -> Result<()>;
    async fn update(&self, submission: EthTxSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<EthTxSubmission>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::storage::{in_memory_db::InMemoryStorage, sled_db::SledDb};

    #[tokio::test]
    async fn can_insert_and_find_latest_block() {
        let test = |storage: Box<dyn Storage>| async move {
            let latest_block = EthTxSubmission {
                fuel_block_height: 10,
                status: EthTxStatus::Pending,
                tx_hash: H256::default(),
            };
            storage.insert(latest_block.clone()).await.unwrap();
            storage
                .insert(EthTxSubmission {
                    fuel_block_height: 9,
                    status: EthTxStatus::Pending,
                    tx_hash: H256::default(),
                })
                .await
                .unwrap();

            let actual = storage.submission_w_latest_block().await.unwrap().unwrap();

            assert_eq!(actual, latest_block);
        };

        test(Box::new(InMemoryStorage::new())).await;
        test(Box::new(SledDb::temporary().unwrap())).await;
    }

    #[tokio::test]
    async fn can_update_block() {
        let test = |storage: Box<dyn Storage>| async move {
            let mut latest_block = EthTxSubmission {
                fuel_block_height: 10,
                status: EthTxStatus::Pending,
                tx_hash: H256::default(),
            };
            storage.insert(latest_block.clone()).await.unwrap();
            latest_block.status = EthTxStatus::Commited;
            storage.update(latest_block.clone()).await.unwrap();

            let actual = storage.submission_w_latest_block().await.unwrap().unwrap();

            assert_eq!(actual, latest_block);
        };
        test(Box::new(InMemoryStorage::new())).await;
        test(Box::new(SledDb::temporary().unwrap())).await;
    }
}
