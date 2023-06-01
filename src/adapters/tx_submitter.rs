use async_trait::async_trait;
use fuels::types::block::Block as FuelBlock;

use crate::errors::Result;

#[async_trait]
pub trait TxSubmitter {
    async fn submit(&self, block: FuelBlock) -> Result<u64>; //TODO: change to eth tx_id type
}

pub struct FakeTxSubmitter {}

#[async_trait]
impl TxSubmitter for FakeTxSubmitter {
    async fn submit(&self, _block: FuelBlock) -> Result<u64> {
        todo!()
    }
}
