use std::cmp::max;

use futures::TryStreamExt;
use ports::{fuel::FuelBlock, storage::Storage, types::NonEmptyVec};
use tracing::info;
use validator::Validator;

use crate::{Error, Result, Runner};

/// The `BlockImporter` is responsible for importing blocks from the Fuel blockchain
/// into local storage. It fetches blocks from the Fuel API, validates them,
/// and stores them if they are not already present.
pub struct BlockImporter<Db, FuelApi, BlockValidator> {
    storage: Db,
    fuel_api: FuelApi,
    block_validator: BlockValidator,
    starting_height: u32,
}

impl<Db, FuelApi, BlockValidator> BlockImporter<Db, FuelApi, BlockValidator> {
    /// Creates a new `BlockImporter`.
    pub fn new(
        storage: Db,
        fuel_api: FuelApi,
        block_validator: BlockValidator,
        starting_height: u32,
    ) -> Self {
        Self {
            storage,
            fuel_api,
            block_validator,
            starting_height,
        }
    }
}

impl<Db, FuelApi, BlockValidator> BlockImporter<Db, FuelApi, BlockValidator>
where
    Db: Storage,
    FuelApi: ports::fuel::Api,
    BlockValidator: Validator,
{
    /// Fetches and validates the latest block from the Fuel API.
    async fn fetch_latest_block(&self) -> Result<FuelBlock> {
        let latest_block = self.fuel_api.latest_block().await?;

        self.block_validator.validate(&latest_block)?;

        Ok(latest_block)
    }

    /// Imports a block into storage if it's not already available.
    async fn import_block(&self, block: FuelBlock) -> Result<()> {
        let block_id = block.id;
        let block_height = block.header.height;

        if !self.storage.is_block_available(&block_id).await? {
            let db_block = encode_block(&block)?;

            self.storage.insert_block(db_block).await?;

            info!("Imported block: height: {block_height}, id: {block_id}");
        } else {
            info!("Block already available: height: {block_height}, id: {block_id}",);
        }
        Ok(())
    }
}

pub(crate) fn encode_block(block: &FuelBlock) -> Result<ports::storage::FuelBlock> {
    let data = encode_block_data(block)?;
    Ok(ports::storage::FuelBlock {
        hash: *block.id,
        height: block.header.height,
        data,
    })
}

fn encode_block_data(block: &FuelBlock) -> Result<NonEmptyVec<u8>> {
    // added this because genesis block has no transactions and we must have some
    let mut encoded = block.transactions.len().to_be_bytes().to_vec();

    let tx_bytes = block.transactions.iter().flat_map(|tx| tx.iter()).cloned();
    encoded.extend(tx_bytes);

    let data = NonEmptyVec::try_from(encoded)
        .map_err(|e| Error::Other(format!("Couldn't encode block (id:{}): {}", block.id, e)))?;

    Ok(data)
}

impl<Db, FuelApi, BlockValidator> Runner for BlockImporter<Db, FuelApi, BlockValidator>
where
    Db: Storage + Send + Sync,
    FuelApi: ports::fuel::Api + Send + Sync,
    BlockValidator: Validator + Send + Sync,
{
    /// Runs the block importer, fetching and importing blocks as needed.
    async fn run(&mut self) -> Result<()> {
        let available_blocks = self.storage.available_blocks().await?;

        let latest_block = self.fetch_latest_block().await?;

        let chain_height = latest_block.header.height;

        if let Some(db_height_range) = &available_blocks {
            let latest_db_block = *db_height_range.end();
            if latest_db_block > chain_height {
                let err_msg = format!(
                    "Latest database block ({latest_db_block}) is has a height greater than the current chain height ({chain_height})",
                );
                return Err(Error::Other(err_msg));
            }

            if latest_db_block == chain_height {
                info!(
                    "Database is up to date with the chain({chain_height}); no import necessary."
                );
                return Ok(());
            }
        }

        let start_request_range = match available_blocks {
            Some(db_height) => max(self.starting_height, db_height.end().saturating_add(1)),
            None => self.starting_height,
        };

        self.fuel_api
            .blocks_in_height_range(start_request_range..=chain_height)
            .map_err(crate::Error::from)
            .try_for_each(|blocks_batch| async {
                for block in blocks_batch {
                    self.import_block(block).await?;
                }

                Ok(())
            })
            .await?;

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
        test_utils::{self, Blocks, ImportedBlocks},
        Error,
    };

    use super::*;

    fn given_secret_key() -> SecretKey {
        let mut rng = StdRng::seed_from_u64(42);
        SecretKey::random(&mut rng)
    }

    #[tokio::test]
    async fn imports_first_block_when_db_is_empty() -> Result<()> {
        // Given
        let setup = test_utils::Setup::init().await;

        let secret_key = given_secret_key();
        let block = test_utils::mocks::fuel::generate_block(0, &secret_key, 1);

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(vec![block.clone()]);
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 0);

        // When
        importer.run().await?;

        // Then
        let all_blocks = setup
            .db()
            .lowest_sequence_of_unbundled_blocks(0, 10)
            .await?
            .unwrap();

        let expected_block = encode_block(&block)?;

        assert_eq!(**all_blocks, vec![expected_block]);

        Ok(())
    }

    #[tokio::test]
    async fn does_not_request_or_import_blocks_already_in_db() -> Result<()> {
        // Given
        let setup = test_utils::Setup::init().await;

        let ImportedBlocks {
            fuel_blocks: existing_blocks,
            secret_key,
            ..
        } = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..3,
                tx_per_block: 1,
            })
            .await;

        let new_blocks =
            (3..=5).map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key, 1));

        let all_blocks = existing_blocks
            .into_iter()
            .chain(new_blocks.clone())
            .collect_vec();

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(new_blocks.clone());
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 0);

        // When
        importer.run().await?;

        // Then
        let stored_blocks = setup
            .db()
            .lowest_sequence_of_unbundled_blocks(0, 100)
            .await?
            .unwrap();
        let expected_blocks = all_blocks
            .iter()
            .map(|block| encode_block(block).unwrap())
            .collect_vec();

        pretty_assertions::assert_eq!(**stored_blocks, expected_blocks);

        Ok(())
    }

    #[tokio::test]
    async fn fails_if_db_height_is_greater_than_chain_height() -> Result<()> {
        // Given
        let setup = test_utils::Setup::init().await;

        let secret_key = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..6,
                tx_per_block: 1,
            })
            .await
            .secret_key;

        let chain_blocks = (0..=2)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key, 1))
            .collect_vec();

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(chain_blocks.clone());
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 0);

        // When
        let result = importer.run().await;

        // Then
        if let Err(Error::Other(err)) = result {
            assert_eq!(err, "Latest database block (5) is has a height greater than the current chain height (2)");
        } else {
            panic!("Expected an Error::Other due to db height being greater than chain height");
        }

        Ok(())
    }

    #[tokio::test]
    async fn respects_height_even_if_blocks_before_are_missing() -> Result<()> {
        // Given
        let setup = test_utils::Setup::init().await;

        let ImportedBlocks { secret_key, .. } = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..3,
                tx_per_block: 1,
            })
            .await;

        let starting_height = 8;
        let new_blocks = (starting_height..=13)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key, 1))
            .collect_vec();

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(new_blocks.clone());
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer =
            BlockImporter::new(setup.db(), fuel_mock, block_validator, starting_height);

        // When
        importer.run().await?;

        // Then
        let stored_new_blocks = setup
            .db()
            .lowest_sequence_of_unbundled_blocks(starting_height, 100)
            .await?
            .unwrap();
        let expected_blocks = new_blocks
            .iter()
            .map(|block| encode_block(block).unwrap())
            .collect_vec();

        pretty_assertions::assert_eq!(**stored_new_blocks, expected_blocks);

        Ok(())
    }

    #[tokio::test]
    async fn handles_chain_with_no_new_blocks() -> Result<()> {
        // Given
        let setup = test_utils::Setup::init().await;

        let ImportedBlocks {
            fuel_blocks,
            storage_blocks,
            secret_key,
            ..
        } = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..3,
                tx_per_block: 1,
            })
            .await;

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(fuel_blocks);
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 0);

        // When
        importer.run().await?;

        // Then
        // Database should remain unchanged
        let stored_blocks = setup
            .db()
            .lowest_sequence_of_unbundled_blocks(0, 10)
            .await?
            .unwrap();

        assert_eq!(**stored_blocks, storage_blocks);

        Ok(())
    }
}
