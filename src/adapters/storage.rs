use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use ethers::types::H256;

use crate::{common::EthTxStatus, errors::Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EthTxSubmission {
    fuel_block_height: u64,
    status: EthTxStatus,
    tx_hash: H256,
}

#[async_trait]
pub trait Storage {
    async fn insert(&self, submission: EthTxSubmission) -> Result<()>;
    async fn update(&self, entry: EthTxSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<EthTxSubmission>>;
}

pub struct InMemoryStorage {
    pub storage: Arc<Mutex<HashMap<u64, EthTxSubmission>>>,
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self {
            storage: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Storage for InMemoryStorage {
    async fn insert(&self, submission: EthTxSubmission) -> Result<()> {
        self.storage
            .lock()
            .unwrap()
            .insert(submission.fuel_block_height, submission);
        Ok(())
    }

    async fn update(&self, entry: EthTxSubmission) -> Result<()> {
        self.storage
            .lock()
            .unwrap()
            .insert(entry.fuel_block_height, entry);
        Ok(())
    }

    async fn submission_w_latest_block(&self) -> Result<Option<EthTxSubmission>> {
        let res = self
            .storage
            .lock()
            .unwrap()
            .iter()
            .max_by_key(|(k, _)| k.clone())
            .map(|(_, v)| v.clone())
            .clone();
        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn can_insert_and_find_latest_block() {
        let storage = InMemoryStorage::new();

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
    }

    #[tokio::test]
    async fn can_update_block() {
        let storage = InMemoryStorage::new();

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
    }
}
