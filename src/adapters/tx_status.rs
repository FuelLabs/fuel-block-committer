use crate::errors::Result;
use async_trait::async_trait;

use crate::common::EthTxStatus;

#[async_trait]
pub trait TxStatusProvider {
    //TODO: change id to eth type
    async fn tx_status(&self, id: u64) -> Result<EthTxStatus>;
}

pub struct FakeTxStatusProvider {}

#[async_trait]
impl TxStatusProvider for FakeTxStatusProvider {
    async fn tx_status(&self, _id: u64) -> Result<EthTxStatus> {
        todo!()
    }
}
