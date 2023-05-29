use std::path::Path;

use sled::{Config, Db};

use super::{EthTxSubmission, Storage};
use crate::errors::Result;

#[derive(Clone)]
pub struct SledDb {
    db: Db,
}

impl SledDb {
    pub fn open(path: &Path) -> Result<Self> {
        let cache_in_mb = 300;
        let config = Config::new()
            .path(path)
            .cache_capacity(cache_in_mb * 1024 * 1024)
            .mode(sled::Mode::LowSpace)
            .use_compression(true);

        Ok(Self { db: config.open()? })
    }

    pub fn temporary() -> Result<Self> {
        Ok(Self {
            db: Config::new().temporary(true).open()?,
        })
    }

    fn encode(submission: &EthTxSubmission) -> Result<([u8; 4], Vec<u8>)> {
        let key = submission.fuel_block_height.to_be_bytes();
        let value = serde_json::to_vec(&submission)?;
        Ok((key, value))
    }

    fn decode(value: &[u8]) -> Result<EthTxSubmission> {
        Ok(serde_json::from_slice(value)?)
    }
}

#[async_trait::async_trait]
impl Storage for SledDb {
    async fn insert(&self, submission: EthTxSubmission) -> Result<()> {
        let (key, value) = Self::encode(&submission)?;
        self.db.insert(key, value)?;
        Ok(())
    }

    async fn update(&self, submission: EthTxSubmission) -> Result<()> {
        let (key, value) = Self::encode(&submission)?;

        let mut batch = sled::Batch::default();
        batch.remove(&key);
        batch.insert(&key, value);

        Ok(self.db.apply_batch(batch)?)
    }

    async fn submission_w_latest_block(&self) -> Result<Option<EthTxSubmission>> {
        let Some((_, value)) = self.db.last()? else {
            return Ok(None);
        };

        Ok(Some(Self::decode(&value)?))
    }
}

#[cfg(test)]
mod tests {
    use ethers::types::H256;

    use super::*;
    use crate::common::EthTxStatus;

    #[tokio::test]
    async fn uses_big_endian_encoding_in_keys_for_sort_correctness() {
        let db = SledDb::temporary().unwrap();

        for current_height in 0..=1024 {
            let current_entry = EthTxSubmission {
                fuel_block_height: current_height,
                status: EthTxStatus::Pending,
                tx_hash: H256::default(),
            };
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
}
