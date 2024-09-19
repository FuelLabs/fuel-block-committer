use std::cmp::max;

use async_trait::async_trait;
use futures::TryStreamExt;
use ports::{fuel::FuelBlock, storage::Storage, types::NonEmptyVec};
use tracing::{error, info};
use validator::Validator;

use crate::{Error, Result, Runner};

/// The `BlockImporter` is responsible for importing blocks from the Fuel blockchain
/// into local storage. It fetches blocks from the Fuel API, validates them,
/// and stores them if they are not already present.
pub struct BlockImporter<Db, FuelApi, BlockValidator> {
    storage: Db,
    fuel_api: FuelApi,
    block_validator: BlockValidator,
    lookback_window: u32,
}

impl<Db, FuelApi, BlockValidator> BlockImporter<Db, FuelApi, BlockValidator> {
    /// Creates a new `BlockImporter`.
    pub fn new(
        storage: Db,
        fuel_api: FuelApi,
        block_validator: BlockValidator,
        lookback_window: u32,
    ) -> Self {
        Self {
            storage,
            fuel_api,
            block_validator,
            lookback_window,
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

    /// Calculates the import range based on the chain height and database state.
    fn calculate_import_range(&self, chain_height: u32, db_height: Option<u32>) -> (u32, u32) {
        let import_end = chain_height;

        let import_start = match db_height {
            Some(db_height) => max(
                chain_height.saturating_sub(self.lookback_window) + 1,
                db_height + 1,
            ),
            None => chain_height.saturating_sub(self.lookback_window),
        };

        (import_start, import_end)
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

#[async_trait]
impl<Db, FuelApi, BlockValidator> Runner for BlockImporter<Db, FuelApi, BlockValidator>
where
    Db: Storage + Send + Sync,
    FuelApi: ports::fuel::Api + Send + Sync,
    BlockValidator: Validator + Send + Sync,
{
    /// Runs the block importer, fetching and importing blocks as needed.
    async fn run(&mut self) -> Result<()> {
        if self.lookback_window == 0 {
            info!("lookback_window is zero; skipping import.");
            return Ok(());
        }

        let available_blocks = self.storage.available_blocks().await?;
        let db_empty = available_blocks.is_empty();

        let latest_block = self.fetch_latest_block().await?;

        let chain_height = latest_block.header.height;
        let db_height = if db_empty {
            None
        } else {
            Some(available_blocks.end.saturating_sub(1))
        };

        if let Some(db_height) = db_height {
            if db_height > chain_height {
                let err_msg = format!(
                    "Database height ({}) is greater than chain height ({})",
                    db_height, chain_height
                );
                error!("{}", err_msg);
                return Err(Error::Other(err_msg));
            }

            if db_height == chain_height {
                info!(
                    "Database is up to date with the chain({chain_height}); no import necessary."
                );
                return Ok(());
            }
        }

        let (import_start, import_end) = self.calculate_import_range(chain_height, db_height);

        // We don't include the latest block in the range because we will import it separately.
        if import_start <= import_end {
            self.fuel_api
                .blocks_in_height_range(import_start..import_end)
                .map_err(crate::Error::from)
                .try_for_each(|block| async {
                    self.import_block(block).await?;
                    Ok(())
                })
                .await?;
        }

        // Import the latest block if it's missing or the DB is empty.
        let latest_block_missing = db_height.map_or(true, |db_height| db_height != chain_height);
        if latest_block_missing {
            self.import_block(latest_block).await?;
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

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 10);

        // When
        importer.run().await?;

        // Then
        let all_blocks = setup.db().lowest_unbundled_blocks(10, 10).await?.unwrap();

        let expected_block = encode_block(&block)?;

        assert_eq!(**all_blocks, vec![expected_block]);

        Ok(())
    }

    #[tokio::test]
    async fn does_not_reimport_blocks_already_in_db() -> Result<()> {
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

        let all_blocks = existing_blocks.into_iter().chain(new_blocks).collect_vec();

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(all_blocks.clone());
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 10);

        // When
        importer.run().await?;

        // Then
        let stored_blocks = setup.db().lowest_unbundled_blocks(100, 100).await?.unwrap();
        let expected_blocks = all_blocks
            .iter()
            .map(|block| encode_block(block).unwrap())
            .collect_vec();

        pretty_assertions::assert_eq!(**stored_blocks, expected_blocks);

        Ok(())
    }

    #[tokio::test]
    async fn does_nothing_if_import_depth_is_zero() -> Result<()> {
        // Given
        let setup = test_utils::Setup::init().await;
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let fuel_mock = ports::fuel::MockApi::new();

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 0);

        // When
        importer.run().await?;

        // Then
        // No blocks should have been imported
        let stored_blocks = setup.db().lowest_unbundled_blocks(10, 10).await?;
        assert!(stored_blocks.is_none());

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

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 10);

        // When
        let result = importer.run().await;

        // Then
        if let Err(Error::Other(err)) = result {
            assert_eq!(err, "Database height (5) is greater than chain height (2)");
        } else {
            panic!("Expected an Error::Other due to db height being greater than chain height");
        }

        Ok(())
    }

    #[tokio::test]
    async fn imports_blocks_when_db_is_stale() -> Result<()> {
        // Given
        let setup = test_utils::Setup::init().await;

        let ImportedBlocks {
            fuel_blocks: db_blocks,
            secret_key,
            ..
        } = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..3,
                tx_per_block: 1,
            })
            .await;

        let chain_blocks =
            (3..=5).map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key, 1));

        let all_blocks = db_blocks.into_iter().chain(chain_blocks).collect_vec();

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(all_blocks.clone());
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 10);

        // When
        importer.run().await?;

        // Then
        let stored_blocks = setup.db().lowest_unbundled_blocks(10, 100).await?.unwrap();
        let expected_blocks = all_blocks
            .iter()
            .map(|block| encode_block(block).unwrap())
            .collect_vec();

        assert_eq!(**stored_blocks, expected_blocks);

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

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 10);

        // When
        importer.run().await?;

        // Then
        // Database should remain unchanged
        let stored_blocks = setup.db().lowest_unbundled_blocks(10, 10).await?.unwrap();

        assert_eq!(**stored_blocks, storage_blocks);

        Ok(())
    }

    #[tokio::test]
    async fn imports_full_range_when_db_is_empty_and_depth_exceeds_chain_height() -> Result<()> {
        // Given
        let setup = test_utils::Setup::init().await;

        let secret_key = given_secret_key();
        let blocks = (0..=5)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key, 1))
            .collect_vec();

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(blocks.clone());
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        // Set import_depth greater than chain height
        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 10);

        // When
        importer.run().await?;

        // Then
        let stored_blocks = setup.db().lowest_unbundled_blocks(10, 10).await?.unwrap();
        let expected_blocks = blocks
            .iter()
            .map(|block| encode_block(block).unwrap())
            .collect_vec();

        assert_eq!(**stored_blocks, expected_blocks);

        Ok(())
    }
}
