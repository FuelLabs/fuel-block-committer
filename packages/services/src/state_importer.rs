use async_trait::async_trait;
use ports::{
    fuel::FuelBlock,
    storage::Storage,
    types::{StateFragment, StateSubmission},
};
use validator::Validator;

use crate::{Result, Runner};

pub struct StateImporter<Db, A, BlockValidator> {
    storage: Db,
    fuel_adapter: A,
    block_validator: BlockValidator,
}

impl<Db, A, BlockValidator> StateImporter<Db, A, BlockValidator> {
    pub fn new(storage: Db, fuel_adapter: A, block_validator: BlockValidator) -> Self {
        Self {
            storage,
            fuel_adapter,
            block_validator,
        }
    }
}

impl<Db, A, BlockValidator> StateImporter<Db, A, BlockValidator>
where
    Db: Storage,
    A: ports::fuel::Api,
    BlockValidator: Validator,
{
    async fn fetch_latest_block(&self) -> Result<FuelBlock> {
        let latest_block = self.fuel_adapter.latest_block().await?;

        // validate block but don't return the validated block
        // so we can use the original block for state submission
        self.block_validator.validate(&latest_block)?;

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
        use itertools::Itertools;

        // Serialize the block into bytes
        let fragments = block
            .transactions
            .iter()
            .flat_map(|tx| tx.iter())
            .chunks(StateFragment::MAX_FRAGMENT_SIZE)
            .into_iter()
            .enumerate()
            .map(|(index, chunk)| StateFragment {
                block_hash: *block.id,
                transaction_hash: None,
                fragment_index: index as u32,
                raw_data: chunk.copied().collect(),
                created_at: ports::types::Utc::now(),
                completed: false,
            })
            .collect();

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
impl<Db, Fuel, BlockValidator> Runner for StateImporter<Db, Fuel, BlockValidator>
where
    Db: Storage,
    Fuel: ports::fuel::Api,
    BlockValidator: Validator,
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
    use fuel_crypto::{Message, SecretKey, Signature};
    use ports::fuel::{FuelConsensus, FuelPoAConsensus};
    use rand::{rngs::StdRng, SeedableRng};
    use storage::PostgresProcess;
    use validator::BlockValidator;

    use ports::fuel::{FuelBlockId, FuelHeader, FuelBlock};

    use super::*;

    fn given_secret_key() -> SecretKey {
        let mut rng = StdRng::seed_from_u64(42);

        SecretKey::random(&mut rng)
    }

    fn given_a_block(height: u32, secret_key: &SecretKey) -> FuelBlock {
        let header = given_header(height);

        let mut hasher = fuel_crypto::Hasher::default();
        hasher.input(header.prev_root.as_ref());
        hasher.input(header.height.to_be_bytes());
        hasher.input(header.time.0.to_be_bytes());
        hasher.input(header.application_hash.as_ref());

        let id = FuelBlockId::from(hasher.digest());
        let id_message = Message::from_bytes(*id);
        let signature = Signature::sign(secret_key, &id_message);

        FuelBlock {
            id,
            header,
            consensus: FuelConsensus::PoAConsensus(FuelPoAConsensus { signature }),
            transactions:  vec![[2u8; 32].into()],
            block_producer: Some(secret_key.public_key()),
        }
    }

    fn given_header(height: u32) -> FuelHeader {
        let application_hash = "0x017ab4b70ea129c29e932d44baddc185ad136bf719c4ada63a10b5bf796af91e"
            .parse()
            .unwrap();

        ports::fuel::FuelHeader {
            id: Default::default(),
            da_height: Default::default(),
            consensus_parameters_version: Default::default(),
            state_transition_bytecode_version: Default::default(),
            transactions_count: 1,
            message_receipt_count: Default::default(),
            transactions_root: Default::default(),
            message_outbox_root: Default::default(),
            event_inbox_root: Default::default(),
            height,
            prev_root: Default::default(),
            time: tai64::Tai64(0),
            application_hash,
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
        // given
        let secret_key = given_secret_key();
        let block = given_a_block(1, &secret_key);
        let block_id = *block.id;
        let fuel_mock = given_fetcher(block);
        let block_validator = BlockValidator::new(secret_key.public_key());

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;
        let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator);

        // when
        importer.run().await.unwrap();

        // then
        let fragments = db.get_unsubmitted_fragments().await?;
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].block_hash, block_id);

        Ok(())
    }
}
