use std::ops::{Range, RangeInclusive};

use async_trait::async_trait;
use futures::{stream, StreamExt, TryStreamExt};
use itertools::Itertools;
use ports::{fuel::FuelBlock, storage::Storage, types::StateSubmission};
use tracing::info;
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

        self.block_validator.validate(&latest_block)?;

        Ok(latest_block)
    }

    async fn check_if_imported(&self, hash: &[u8; 32]) -> Result<bool> {
        Ok(self.storage.block_available(hash).await?)
    }

    async fn last_submitted_block_height(&self) -> Result<Option<u32>> {
        Ok(self
            .storage
            .state_submission_w_latest_block()
            .await?
            .map(|submission| submission.block_height))
    }

    async fn import_state(&self, block: FuelBlock) -> Result<()> {
        let block_id = block.id;
        let block_height = block.header.height;
        if !self.storage.block_available(&block_id).await? {
            self.storage.insert_block(block.into()).await?;

            info!("imported state from fuel block: height: {block_height}, id: {block_id}");
        }
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
        let block_roster = self.storage.block_roster().await?;

        let latest_block = self.fetch_latest_block().await?;

        // TODO: segfault the cutoff to be configurable
        let mut missing_blocks = block_roster.missing_block_heights(latest_block.header.height, 0);
        missing_blocks.retain(|height| *height != latest_block.header.height);

        // Everything up to the latest block
        stream::iter(split_into_ranges(missing_blocks))
            .flat_map(|range| self.fuel_adapter.blocks_in_height_range(range))
            .map_err(crate::Error::from)
            .try_for_each(|block| async {
                self.import_state(block).await?;
                Ok(())
            })
            .await?;

        Ok(())
    }
}

fn split_into_ranges(nums: Vec<u32>) -> Vec<Range<u32>> {
    nums.into_iter()
        .sorted()
        .fold(Vec::new(), |mut ranges, num| {
            if let Some((_start, end)) = ranges.last_mut() {
                if num == *end + 1 {
                    // Extend the current range
                    *end = num;
                } else {
                    // Start a new range
                    ranges.push((num, num));
                }
            } else {
                // First range
                ranges.push((num, num));
            }
            ranges
        })
        .into_iter()
        .map(|(begin, end_inclusive)| {
            let end_exclusive = end_inclusive.saturating_add(1);
            begin..end_exclusive
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use fuel_crypto::{Message, SecretKey, Signature};
    use mockall::predicate::eq;
    use ports::fuel::{FuelBlock, FuelBlockId, FuelConsensus, FuelHeader, FuelPoAConsensus};
    use rand::{rngs::StdRng, SeedableRng};
    use storage::PostgresProcess;
    use validator::BlockValidator;

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
            transactions: vec![[2u8; 32].into()],
            block_producer: Some(secret_key.public_key()),
        }
    }

    fn given_header(height: u32) -> FuelHeader {
        let application_hash = "0x8b96f712e293e801d53da77113fec3676c01669c6ea05c6c92a5889fce5f649d"
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

    fn given_streaming_fetcher(block: FuelBlock) -> ports::fuel::MockApi {
        let mut fetcher = ports::fuel::MockApi::new();

        fetcher
            .expect_blocks_in_height_range()
            .with(eq(block.header.height..block.header.height + 1))
            .return_once(move |_| stream::once(async move { Ok(block.clone()) }).boxed());

        fetcher
    }

    fn given_latest_fetcher(block: FuelBlock) -> ports::fuel::MockApi {
        let mut fetcher = ports::fuel::MockApi::new();

        fetcher.expect_latest_block().return_once(move || Ok(block));

        fetcher
    }

    #[tokio::test]
    async fn imports_latest_block_when_no_blocks_are_missing() -> Result<()> {
        // given
        let secret_key = given_secret_key();
        let block = given_a_block(1, &secret_key);
        let fuel_mock = given_latest_fetcher(block.clone());

        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;
        let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator);

        // when
        importer.run().await.unwrap();

        // then
        let all_blocks = db.all_blocks().await?;

        assert_eq!(all_blocks, vec![block.into()]);

        Ok(())
    }

    #[tokio::test]
    async fn skips_import_if_block_imported() -> Result<()> {
        // given
        let secret_key = given_secret_key();
        let block = given_a_block(1, &secret_key);
        let fuel_mock = given_latest_fetcher(block.clone());
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let process = PostgresProcess::shared().await.unwrap();

        let db = process.create_random_db().await?;
        db.insert_block(block.clone().into()).await?;

        let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator);

        // when
        let res = importer.run().await;

        // then
        res.unwrap();
        Ok(())
    }

    #[tokio::test]
    async fn fills_in_missing_blocks_in_middle() -> Result<()> {
        // given
        let secret_key = given_secret_key();
        let block_1 = given_a_block(1, &secret_key);
        let block_2 = given_a_block(2, &secret_key);
        let block_3 = given_a_block(3, &secret_key);
        let block_4 = given_a_block(4, &secret_key);
        let block_5 = given_a_block(5, &secret_key);

        let mut fuel_mock = ports::fuel::MockApi::new();

        let ret = block_2.clone();
        fuel_mock
            .expect_blocks_in_height_range()
            .with(eq(2..3))
            .return_once(move |_| stream::once(async move { Ok(ret) }).boxed());

        let ret = block_4.clone();
        fuel_mock
            .expect_blocks_in_height_range()
            .with(eq(4..5))
            .return_once(move |_| stream::once(async move { Ok(ret) }).boxed());

        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let process = PostgresProcess::shared().await.unwrap();

        let db = process.create_random_db().await?;
        db.insert_block(block_1.clone().into()).await?;
        db.insert_block(block_3.clone().into()).await?;
        db.insert_block(block_5.clone().into()).await?;

        let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator);

        // when
        importer.run().await?;

        // then
        let available_blocks = db.all_blocks().await?;
        assert_eq!(
            available_blocks,
            vec![
                block_1.clone().into(),
                block_2.clone().into(),
                block_3.clone().into(),
                block_4.clone().into(),
                block_5.clone().into()
            ]
        );

        Ok(())
    }

    #[tokio::test]
    async fn fills_in_missing_blocks_at_end() -> Result<()> {
        // given
        let secret_key = given_secret_key();
        let block_1 = given_a_block(1, &secret_key);
        let block_2 = given_a_block(2, &secret_key);
        let block_3 = given_a_block(3, &secret_key);
        let block_4 = given_a_block(4, &secret_key);

        let mut fuel_mock = ports::fuel::MockApi::new();

        let ret = vec![Ok(block_2.clone()), Ok(block_3.clone())];
        fuel_mock
            .expect_blocks_in_height_range()
            .with(eq(2..4))
            .return_once(move |_| stream::iter(ret).boxed());

        let ret = block_4.clone();
        fuel_mock.expect_latest_block().return_once(|| Ok(ret));

        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let process = PostgresProcess::shared().await.unwrap();

        let db = process.create_random_db().await?;
        db.insert_block(block_1.clone().into()).await?;

        let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator);

        // when
        importer.run().await?;

        // then
        let available_blocks = db.all_blocks().await?;
        assert_eq!(
            available_blocks,
            vec![
                block_1.clone().into(),
                block_2.clone().into(),
                block_3.clone().into(),
                block_4.clone().into(),
            ]
        );

        Ok(())
    }

    // #[tokio::test]
    // async fn test_import_state() -> Result<()> {
    //     // given
    //     let secret_key = given_secret_key();
    //     let block = given_a_block(1, &secret_key);
    //     let fuel_mock = given_fetcher(block);
    //     let block_validator = BlockValidator::new(*secret_key.public_key().hash());
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator);
    //
    //     // when
    //     importer.run().await.unwrap();
    //
    //     // then
    //     let fragments = db.stream_unfinalized_segment_data(usize::MAX).await?;
    //     let latest_submission = db.state_submission_w_latest_block().await?.unwrap();
    //     assert_eq!(fragments.len(), 1);
    //     assert_eq!(fragments[0].submission_id, latest_submission.id);
    //
    //     Ok(())
    // }
}
