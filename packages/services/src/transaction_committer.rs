use async_trait::async_trait;
use ports::{fuel::FuelBlock, storage::Storage, types::{StateFragment, StateSubmission}};

use crate::{Result, Runner};

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

    fn block_to_state_submission(&self, block: FuelBlock) -> Result<(StateSubmission, Vec<StateFragment>)> {
        // placeholder data
        let raw_data = vec![1u8; 50000];

        let fragment = StateFragment {
            raw_data,
            fragment_index: 0,
            state_submission: block.header.height,
            is_completed: false,
        };
        
        let submission = StateSubmission {
            fuel_block_height: block.header.height,
            is_completed: false,
            num_fragments: 1,
        };
        
        Ok((submission, vec![fragment]))
    }

    async fn submit_state(&self, block: FuelBlock) -> Result<()> {
        let (submission, fragments) = self.block_to_state_submission(block)?;
        self.storage.insert_state(submission, fragments).await?;
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
        self.submit_state(block).await?;

        Ok(())
    }
}
