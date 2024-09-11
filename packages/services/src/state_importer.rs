use std::{
    cmp::max,
    collections::BTreeSet,
    ops::{Range, RangeInclusive},
};

use async_trait::async_trait;
use futures::{stream, StreamExt, TryStreamExt};
use itertools::Itertools;
use ports::{fuel::FuelBlock, storage::Storage, types::StateSubmission};
use tracing::info;
use validator::Validator;

use crate::{Error, Result, Runner};

// TODO: rename to block importer
pub struct StateImporter<Db, A, BlockValidator> {
    storage: Db,
    fuel_adapter: A,
    block_validator: BlockValidator,
    import_depth: u32,
}

impl<Db, A, BlockValidator> StateImporter<Db, A, BlockValidator> {
    pub fn new(
        storage: Db,
        fuel_adapter: A,
        block_validator: BlockValidator,
        import_depth: u32,
    ) -> Self {
        Self {
            storage,
            fuel_adapter,
            block_validator,
            import_depth,
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
        Ok(self.storage.is_block_available(hash).await?)
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
        if !self.storage.is_block_available(&block_id).await? {
            let db_block = block
                .try_into()
                .map_err(|err| Error::Other(format!("cannot turn block into data: {err}")))?;
            self.storage.insert_block(db_block).await?;

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
        if self.import_depth == 0 {
            return Ok(());
        }

        let available_blocks = self.storage.available_blocks().await?.into_inner();
        let db_empty = available_blocks.is_empty();

        // TODO: segfault check that the latest block is higher than everything we have in the db
        // (out of sync node)
        let latest_block = self.fetch_latest_block().await?;

        let chain_height = latest_block.header.height;
        let db_height = available_blocks.end.saturating_sub(1);

        if !db_empty && db_height > chain_height {
            return Err(Error::Other(format!(
                "db height({}) is greater than chain height({})",
                db_height, chain_height
            )));
        }

        let import_start = if db_empty {
            chain_height.saturating_sub(self.import_depth)
        } else {
            max(
                chain_height
                    .saturating_add(1)
                    .saturating_sub(self.import_depth),
                available_blocks.end,
            )
        };

        // We don't include the latest block in the range because we already have it
        let import_range = import_start..chain_height;

        if !import_range.is_empty() {
            self.fuel_adapter
                .blocks_in_height_range(import_start..chain_height)
                .map_err(crate::Error::from)
                .try_for_each(|block| async {
                    self.import_state(block).await?;
                    Ok(())
                })
                .await?;
        }

        let latest_block_missing = db_height != chain_height;
        if latest_block_missing || db_empty {
            self.import_state(latest_block).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use fuel_crypto::{Message, SecretKey, Signature};
    use mockall::predicate::eq;
    use ports::fuel::{FuelBlock, FuelBlockId, FuelConsensus, FuelHeader, FuelPoAConsensus};
    use rand::{rngs::StdRng, SeedableRng};
    use storage::PostgresProcess;
    use validator::BlockValidator;

    use crate::Error;

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

    fn given_latest_fetcher(block: FuelBlock) -> ports::fuel::MockApi {
        let mut fetcher = ports::fuel::MockApi::new();

        fetcher.expect_latest_block().return_once(move || Ok(block));

        fetcher
    }

    #[tokio::test]
    async fn imports_block_on_empty_db() -> Result<()> {
        // given
        let secret_key = given_secret_key();
        let block = given_a_block(0, &secret_key);
        let fuel_mock = given_latest_fetcher(block.clone());

        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;
        let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator, 1);

        // when
        importer.run().await.unwrap();

        // then
        let all_blocks = db.all_blocks().await?;

        assert_eq!(all_blocks, vec![block.into()]);

        Ok(())
    }

    #[tokio::test]
    async fn shortens_import_depth_if_db_already_has_the_blocks() -> Result<()> {
        // given
        let secret_key = given_secret_key();
        let block_0 = given_a_block(0, &secret_key);
        let block_1 = given_a_block(1, &secret_key);
        let block_2 = given_a_block(2, &secret_key);

        let mut fuel_mock = ports::fuel::MockApi::new();
        let ret = block_1.clone();
        fuel_mock
            .expect_blocks_in_height_range()
            .with(eq(1..2))
            .return_once(move |_| stream::iter(vec![Ok(ret)]).boxed());

        let ret = block_2.clone();
        fuel_mock.expect_latest_block().return_once(|| Ok(ret));

        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let process = PostgresProcess::shared().await.unwrap();

        let db = process.create_random_db().await?;
        db.insert_block(block_0.clone().into()).await?;

        let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator, 3);

        // when
        importer.run().await?;

        // then
        let all_blocks = db.all_blocks().await?;
        assert_eq!(
            all_blocks,
            vec![
                block_0.clone().into(),
                block_1.clone().into(),
                block_2.clone().into()
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn does_nothing_if_depth_is_0() -> Result<()> {
        // given
        let secret_key = given_secret_key();
        let fuel_mock = ports::fuel::MockApi::new();

        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let process = PostgresProcess::shared().await.unwrap();

        let db = process.create_random_db().await?;

        let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator, 0);

        // when
        importer.run().await?;

        // then
        // mocks didn't fail since we didn't call them
        Ok(())
    }

    #[tokio::test]
    async fn fails_if_db_height_is_greater_than_chain_height() -> Result<()> {
        // given
        let secret_key = given_secret_key();
        let db_block = given_a_block(10, &secret_key);
        let chain_block = given_a_block(2, &secret_key);
        let fuel_mock = given_latest_fetcher(chain_block);

        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let process = PostgresProcess::shared().await.unwrap();

        let db = process.create_random_db().await?;
        db.insert_block(db_block.clone().into()).await?;

        let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator, 1);

        // when
        let result = importer.run().await;

        // then
        let Err(Error::Other(err)) = result else {
            panic!("Expected an Error::Other, got: {:?}", result);
        };

        assert_eq!(err, "db height(10) is greater than chain height(2)");
        Ok(())
    }

    #[tokio::test]
    async fn imports_on_very_stale_db() -> Result<()> {
        // given
        let secret_key = given_secret_key();
        let db_block = given_a_block(0, &secret_key);
        let chain_block_11 = given_a_block(11, &secret_key);
        let chain_block_12 = given_a_block(12, &secret_key);
        let mut fuel_mock = ports::fuel::MockApi::new();

        let ret = vec![Ok(chain_block_11.clone())];
        fuel_mock
            .expect_blocks_in_height_range()
            .with(eq(11..12))
            .return_once(move |_| stream::iter(ret).boxed());

        let ret = chain_block_12.clone();
        fuel_mock.expect_latest_block().return_once(|| Ok(ret));

        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let process = PostgresProcess::shared().await.unwrap();

        let db = process.create_random_db().await?;
        db.insert_block(db_block.clone().into()).await?;

        let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator, 2);

        // when
        importer.run().await?;

        // then
        let all_blocks = db.all_blocks().await?;
        assert_eq!(
            all_blocks,
            vec![
                db_block.clone().into(),
                chain_block_11.clone().into(),
                chain_block_12.clone().into()
            ]
        );

        Ok(())
    }

    //
    // #[tokio::test]
    // async fn fills_in_missing_blocks_at_end() -> Result<()> {
    //     // given
    //     let secret_key = given_secret_key();
    //     let block_1 = given_a_block(1, &secret_key);
    //     let block_2 = given_a_block(2, &secret_key);
    //     let block_3 = given_a_block(3, &secret_key);
    //     let block_4 = given_a_block(4, &secret_key);
    //
    //     let mut fuel_mock = ports::fuel::MockApi::new();
    //
    //     let ret = vec![Ok(block_2.clone()), Ok(block_3.clone())];
    //     fuel_mock
    //         .expect_blocks_in_height_range()
    //         .with(eq(2..=3))
    //         .return_once(move |_| stream::iter(ret).boxed());
    //
    //     let ret = block_4.clone();
    //     fuel_mock.expect_latest_block().return_once(|| Ok(ret));
    //
    //     let block_validator = BlockValidator::new(*secret_key.public_key().hash());
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //
    //     let db = process.create_random_db().await?;
    //     db.insert_block(block_1.clone().into()).await?;
    //
    //     let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator, 0);
    //
    //     // when
    //     importer.run().await?;
    //
    //     // then
    //     let available_blocks = db.all_blocks().await?;
    //     assert_eq!(
    //         available_blocks,
    //         vec![
    //             block_1.clone().into(),
    //             block_2.clone().into(),
    //             block_3.clone().into(),
    //             block_4.clone().into(),
    //         ]
    //     );
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn if_no_blocks_available() -> Result<()> {
    //     // given
    //     let secret_key = given_secret_key();
    //     let block_1 = given_a_block(1, &secret_key);
    //     let block_2 = given_a_block(2, &secret_key);
    //     let block_3 = given_a_block(3, &secret_key);
    //     let block_4 = given_a_block(4, &secret_key);
    //
    //     let mut fuel_mock = ports::fuel::MockApi::new();
    //
    //     let ret = vec![Ok(block_2.clone()), Ok(block_3.clone())];
    //     fuel_mock
    //         .expect_blocks_in_height_range()
    //         .with(eq(2..=3))
    //         .return_once(move |_| stream::iter(ret).boxed());
    //
    //     let ret = block_4.clone();
    //     fuel_mock.expect_latest_block().return_once(|| Ok(ret));
    //
    //     let block_validator = BlockValidator::new(*secret_key.public_key().hash());
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //
    //     let db = process.create_random_db().await?;
    //     db.insert_block(block_1.clone().into()).await?;
    //
    //     let mut importer = StateImporter::new(db.clone(), fuel_mock, block_validator, 0);
    //
    //     // when
    //     importer.run().await?;
    //
    //     // then
    //     let available_blocks = db.all_blocks().await?;
    //     assert_eq!(
    //         available_blocks,
    //         vec![
    //             block_1.clone().into(),
    //             block_2.clone().into(),
    //             block_3.clone().into(),
    //             block_4.clone().into(),
    //         ]
    //     );
    //
    //     Ok(())
    // }
}
