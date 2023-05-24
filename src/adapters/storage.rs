use crate::errors::Result;
use async_trait::async_trait;
use fuels::tx::Bytes32;

use crate::common::EthTxStatus;

pub struct EthTxSubmission {
    status: EthTxStatus,
    tx_id: u64, //TODO: change to ETH tx id type
    fuel_block_height: u64,
}

pub struct StorageEntry {
    id: u64,
    value: EthTxSubmission,
}

#[async_trait]
pub trait Storage {
    async fn insert(&self, submission: EthTxSubmission) -> Result<u64>;
    async fn update(&self, entry: StorageEntry) -> Result<()>;
    async fn last_entry(&self) -> Result<Option<StorageEntry>>;
}

pub struct FakeStorage {}

#[async_trait]
impl Storage for FakeStorage {
    async fn insert(&self, submission: EthTxSubmission) -> Result<u64> {
        todo!()
    }
    async fn update(&self, entry: StorageEntry) -> Result<()> {
        todo!()
    }
    async fn last_entry(&self) -> Result<Option<StorageEntry>> {
        todo!()
    }
}
