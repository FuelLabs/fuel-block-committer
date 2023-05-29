use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;

use super::{EthTxSubmission, Storage};
use crate::errors::Result;
#[derive(Clone)]
pub struct InMemoryStorage {
    pub storage: Arc<Mutex<HashMap<u32, EthTxSubmission>>>,
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
            .max_by_key(|(k, _)| *k)
            .map(|(_, v)| v.clone());
        Ok(res)
    }
}
