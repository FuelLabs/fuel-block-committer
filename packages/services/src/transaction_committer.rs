use async_trait::async_trait;
use ports::{fuel::FuelBlock, storage::Storage, types::{StateFragment, StateSubmission}};

use crate::{Result, Runner};

pub struct TransactionCommitter<Db, A> {
    storage: Db,
    fuel_adapter: A,
}

impl<Db, A> TransactionCommitter<Db, A> {
    pub fn new(storage: Db, fuel_adapter: A) -> Self {
        Self {
            storage,
            fuel_adapter,
        }
    }
}

impl<Db, A> TransactionCommitter<Db, A>
where
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
            completed: false,
            block_hash: *block.id,
        };
        
        let submission = StateSubmission {
            block_hash: *block.header.id,
            block_height: block.header.height,
            completed: false,
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
impl<Db, Fuel> Runner for TransactionCommitter<Db, Fuel>
where
    Db: Storage,
    Fuel: ports::fuel::Api,
{
    async fn run(&mut self) -> Result<()> {
        let block = self.fetch_latest_block().await?;
        self.submit_state(block).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ports::fuel::{FuelBytes32, MockApi as FuelMockApi};
    use storage::PostgresProcess;
    use tai64::Tai64;

    use super::*;

    #[tokio::test]
    async fn test_submit_state() -> Result<()> {
        let fuel_mock = FuelMockApi::new();

        let id = FuelBytes32::from([1u8; 32]);
        let header = ports::fuel::FuelHeader {
            id,
            da_height: 0,
            consensus_parameters_version: Default::default(),
            state_transition_bytecode_version: Default::default(),
            transactions_count: 1,
            message_receipt_count: 0,
            transactions_root: Default::default(),
            message_outbox_root: Default::default(),
            event_inbox_root: Default::default(),
            height: 1,
            prev_root: Default::default(),
            time: Tai64::now(),
            application_hash: Default::default(),
        };
        let block = FuelBlock {
            id,
            header,
            transactions: vec![],
            consensus: ports::fuel::FuelConsensus::Unknown,
            block_producer: Default::default(),
        };

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;
        let committer = TransactionCommitter::new(db, fuel_mock);
        
        committer.submit_state(block).await.unwrap();

        Ok(())
    }
}
