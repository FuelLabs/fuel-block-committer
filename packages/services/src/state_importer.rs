use async_trait::async_trait;
use ports::{
    fuel::FuelBlock,
    storage::Storage,
    types::{StateFragment, StateSubmission},
};

use crate::{Result, Runner};

pub struct StateImporter<Db, A> {
    storage: Db,
    fuel_adapter: A,
}

impl<Db, A> StateImporter<Db, A> {
    pub fn new(storage: Db, fuel_adapter: A) -> Self {
        Self {
            storage,
            fuel_adapter,
        }
    }
}

impl<Db, A> StateImporter<Db, A>
where
    Db: Storage,
    A: ports::fuel::Api,
{
    async fn fetch_latest_block(&self) -> Result<FuelBlock> {
        let latest_block = self.fuel_adapter.latest_block().await?;
        // validate if needed

        Ok(latest_block)
    }

    async fn check_if_stale(&self, block_height: u32) -> Result<bool> {
        let Some(submitted_height) = self.last_submitted_block_height().await? else {
            return Ok(false);
        };

        Ok(submitted_height >= block_height)
    }

    async fn last_submitted_block_height(&self) -> Result<Option<u32>> {
        Ok(self
            .storage
            .state_submission_w_latest_block()
            .await?
            .map(|submission| submission.block_height))
    }

    fn block_to_state_submission(
        &self,
        block: FuelBlock,
    ) -> Result<(StateSubmission, Vec<StateFragment>)> {
        // Serialize the block into bytes
        let block_data = block
            .transactions
            .iter()
            .flat_map(|tx| tx.to_vec())
            .collect::<Vec<_>>();

        let fragments = block_data
            .chunks(StateFragment::MAX_FRAGMENT_SIZE)
            .enumerate()
            .map(|(index, chunk)| StateFragment {
                block_hash: *block.id,
                transaction_hash: None,
                fragment_index: index as u32,
                raw_data: chunk.to_vec(),
                completed: false,
            })
            .collect::<Vec<StateFragment>>();

        let submission = StateSubmission {
            block_hash: *block.id,
            block_height: block.header.height,
            completed: false,
        };

        Ok((submission, fragments))
    }

    async fn import_state(&self, block: FuelBlock) -> Result<()> {
        let (submission, fragments) = self.block_to_state_submission(block)?;
        self.storage.insert_state(submission, fragments).await?;

        Ok(())
    }
}

#[async_trait]
impl<Db, Fuel> Runner for StateImporter<Db, Fuel>
where
    Db: Storage,
    Fuel: ports::fuel::Api,
{
    async fn run(&mut self) -> Result<()> {
        let block = self.fetch_latest_block().await?;

        if self.check_if_stale(block.header.height).await? {
            return Ok(());
        }

        if block.transactions.is_empty() {
            return Ok(());
        }

        self.import_state(block).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ports::fuel::FuelBytes32;
    use storage::PostgresProcess;
    use tai64::Tai64;

    use super::*;

    fn given_block() -> FuelBlock {
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

        FuelBlock {
            id,
            header,
            transactions: vec![[2u8; 32].into()],
            consensus: ports::fuel::FuelConsensus::Unknown,
            block_producer: Default::default(),
        }
    }

    fn given_fetcher(block: FuelBlock) -> ports::fuel::MockApi {
        let mut fetcher = ports::fuel::MockApi::new();

        fetcher
            .expect_latest_block()
            .returning(move || Ok(block.clone()));

        fetcher
    }

    #[tokio::test]
    async fn test_import_state() -> Result<()> {
        let block = given_block();
        let block_id = *block.id;
        let fuel_mock = given_fetcher(block);

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;
        let mut importer = StateImporter::new(db.clone(), fuel_mock);

        importer.run().await.unwrap();

        let fragments = db.get_unsubmitted_fragments().await?;
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].block_hash, block_id);

        Ok(())
    }
}
