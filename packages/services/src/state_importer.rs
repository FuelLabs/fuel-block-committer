use std::cmp::max;

use async_trait::async_trait;
use futures::TryStreamExt;
use ports::{fuel::FuelBlock, storage::Storage, types::NonEmptyVec};
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

    async fn import_state(&self, block: FuelBlock) -> Result<()> {
        let block_id = block.id;
        let block_height = block.header.height;
        if !self.storage.is_block_available(&block_id).await? {
            let db_block = ports::storage::FuelBlock {
                hash: *block_id,
                height: block_height,
                data: encode_block_data(block)?,
            };

            self.storage.insert_block(db_block).await?;

            info!("imported state from fuel block: height: {block_height}, id: {block_id}");
        }
        Ok(())
    }
}

pub(crate) fn encode_block_data(block: FuelBlock) -> Result<NonEmptyVec<u8>> {
    let tx_bytes: Vec<u8> = block
        .transactions
        .into_iter()
        .flat_map(|tx| tx.into_iter())
        .collect();

    let data = NonEmptyVec::try_from(tx_bytes)
        .map_err(|e| Error::Other(format!("couldn't encode block (id:{}): {e} ", block.id)))?;

    Ok(data)
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
    use fuel_crypto::SecretKey;
    use itertools::Itertools;
    use rand::{rngs::StdRng, SeedableRng};
    use validator::BlockValidator;

    use crate::{
        test_utils::{self, Blocks},
        Error,
    };

    use super::*;

    #[tokio::test]
    async fn imports_block_on_empty_db() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let secret_key = given_secret_key();
        let block = test_utils::mocks::fuel::generate_block(0, &secret_key);

        let mut sut = setup.importer_of_blocks(Blocks::Blocks {
            blocks: vec![block.clone()],
            secret_key,
        });

        // when
        sut.run().await.unwrap();

        // then
        let all_blocks = setup.db().lowest_unbundled_blocks(10).await?;

        let expected_block = ports::storage::FuelBlock {
            height: 0,
            hash: *block.id,
            data: encode_block_data(block)?,
        };

        assert_eq!(all_blocks, vec![expected_block]);

        Ok(())
    }

    #[tokio::test]
    async fn doesnt_ask_for_blocks_it_already_has() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;
        let secret_key = given_secret_key();
        let previously_imported = (0..=2)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();

        setup
            .import_blocks(Blocks::Blocks {
                blocks: previously_imported.clone(),
                secret_key,
            })
            .await;

        let new_blocks = (3..=5)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();

        let mut sut = setup.importer_of_blocks(Blocks::Blocks {
            blocks: new_blocks.clone(),
            secret_key,
        });

        // when
        sut.run().await?;

        // then
        // the fuel mock generated by the helpers above has a check for tightness of the asked
        // block range. If we ask for blocks outside of what we gave in Blocks::Blocks it will fail.

        let all_blocks = setup.db().lowest_unbundled_blocks(100).await?;
        let expected_blocks = previously_imported
            .iter()
            .chain(new_blocks.iter())
            .map(|block| ports::storage::FuelBlock {
                height: block.header.height,
                hash: *block.id,
                data: encode_block_data(block.clone()).unwrap(),
            })
            .collect_vec();

        assert_eq!(all_blocks, expected_blocks);
        Ok(())
    }

    #[tokio::test]
    async fn does_nothing_if_depth_is_0() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut sut =
            StateImporter::new(setup.db(), ports::fuel::MockApi::new(), block_validator, 0);

        // when
        sut.run().await?;

        // then
        // mocks didn't fail since we didn't call them
        Ok(())
    }

    #[tokio::test]
    async fn fails_if_db_height_is_greater_than_chain_height() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        setup.import_blocks(Blocks::WithHeights(0..5)).await;

        let secret_key = given_secret_key();
        let new_blocks = (0..=2)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(new_blocks);
        let mut sut = StateImporter::new(setup.db(), fuel_mock, block_validator, 1);

        // when
        let result = sut.run().await;

        // then
        let Err(Error::Other(err)) = result else {
            panic!("Expected an Error::Other, got: {:?}", result);
        };

        assert_eq!(err, "db height(4) is greater than chain height(2)");
        Ok(())
    }

    #[tokio::test]
    async fn imports_on_very_stale_db() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let old_blocks = (0..=2)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &given_secret_key()))
            .collect_vec();

        setup
            .import_blocks(Blocks::Blocks {
                blocks: old_blocks.clone(),
                secret_key: given_secret_key(),
            })
            .await;

        let secret_key = given_secret_key();

        let new_blocks = (8..=10)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();
        let mut sut = setup.importer_of_blocks(Blocks::Blocks {
            blocks: new_blocks.clone(),
            secret_key,
        });

        // when
        sut.run().await?;

        // then
        let all_blocks = setup.db().lowest_unbundled_blocks(100).await?;
        let expected_blocks = old_blocks
            .iter()
            .chain(new_blocks.iter())
            .map(|block| ports::storage::FuelBlock {
                height: block.header.height,
                hash: *block.id,
                data: encode_block_data(block.clone()).unwrap(),
            })
            .collect_vec();

        assert_eq!(all_blocks, expected_blocks);

        Ok(())
    }

    fn given_secret_key() -> SecretKey {
        let mut rng = StdRng::seed_from_u64(42);

        SecretKey::random(&mut rng)
    }
}
