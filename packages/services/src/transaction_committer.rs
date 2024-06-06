use async_trait::async_trait;
use ports::{fuel::FuelBlock, storage::Storage};

use crate::{Runner, Error, Result};

pub struct TransactionCommitter<L1, Db, A> {
    l1_adapter: L1,
    storage: Db,
    fuel_adapter: A,
}

impl<L1, Db, A> TransactionCommitter<L1, Db, A> {
    pub fn new(l1: L1, storage: Db, fuel_adapter: A) -> Self {
        Self {
            l1_adapter: l1,
            storage,
            fuel_adapter,
        }
    }
}

impl<L1, Db, A> TransactionCommitter<L1, Db, A>
where
    L1: ports::l1::Api,
    Db: Storage,
    A: ports::fuel::Api,
{
    async fn fetch_latest_block(&self) -> Result<FuelBlock> {
        let latest_block = self.fuel_adapter.latest_block().await?;

        // validate if needed

        Ok(latest_block)
    }

    async fn submit_block(&self, fuel_block: FuelBlock) -> Result<()> {
        let submittal_height = self.l1_adapter.get_block_number().await?;

        // register submission in DB

        /// blob adapter submit
        //self.l1_adapter

        Ok(())
    }
}

#[async_trait]
impl<L1, Db, Fuel> Runner for TransactionCommitter<L1, Db, Fuel>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage,
    Fuel: ports::fuel::Api,
{
    async fn run(&mut self) -> Result<()> {
        let block = self.fetch_latest_block().await?;
        self.submit_block(block).await?;

        Ok(())
    }
}
