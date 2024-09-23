use std::cmp::max;

use futures::TryStreamExt;
use itertools::chain;
use ports::{
    fuel::FullFuelBlock,
    storage::Storage,
    types::{CollectNonEmpty, NonEmpty},
};
use tracing::info;

use crate::{validator::Validator, Error, Result, Runner};

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
    /// Imports a block into storage if it's not already available.
    async fn import_blocks(&self, blocks: NonEmpty<FullFuelBlock>) -> Result<()> {
        let db_blocks = encode_blocks(blocks);

        let starting_height = db_blocks.first().height;
        let ending_height = db_blocks.last().height;

        self.storage.insert_blocks(db_blocks).await?;

        info!("Imported blocks: {starting_height}..={ending_height}");

        Ok(())
    }

    fn validate_blocks(&self, blocks: &NonEmpty<FullFuelBlock>) -> Result<()> {
        for block in blocks {
            self.block_validator
                .validate(block.id, &block.header, &block.consensus)?;
        }

        Ok(())
    }

    async fn determine_starting_height(&mut self, chain_height: u32) -> Result<Option<u32>> {
        let Some(available_blocks) = self.storage.available_blocks().await? else {
            return Ok(Some(self.starting_height));
        };

        let latest_db_block = *available_blocks.end();

        match latest_db_block.cmp(&chain_height) {
            std::cmp::Ordering::Greater => {
                let err_msg = format!(
                    "Latest database block ({latest_db_block}) is has a height greater than the current chain height ({chain_height})",
                );
                Err(Error::Other(err_msg))
            }
            std::cmp::Ordering::Equal => Ok(None),
            std::cmp::Ordering::Less => Ok(Some(max(
                self.starting_height,
                latest_db_block.saturating_add(1),
            ))),
        }
    }
}

pub(crate) fn encode_blocks(
    blocks: NonEmpty<FullFuelBlock>,
) -> NonEmpty<ports::storage::FuelBlock> {
    blocks
        .into_iter()
        .map(|full_block| ports::storage::FuelBlock {
            hash: *full_block.id,
            height: full_block.header.height,
            data: encode_block_data(full_block),
        })
        .collect_nonempty()
        .expect("should be non-empty")
}

fn encode_block_data(block: FullFuelBlock) -> NonEmpty<u8> {
    let tx_num = u64::try_from(block.raw_transactions.len()).unwrap_or(u64::MAX);

    chain!(
        tx_num.to_be_bytes(),
        block.raw_transactions.into_iter().flatten()
    )
    .collect_nonempty()
    .expect("should be non-empty")
}

impl<Db, FuelApi, BlockValidator> Runner for BlockImporter<Db, FuelApi, BlockValidator>
where
    Db: Storage + Send + Sync,
    FuelApi: ports::fuel::Api + Send + Sync,
    BlockValidator: Validator + Send + Sync,
{
    /// Runs the block importer, fetching and importing blocks as needed.
    async fn run(&mut self) -> Result<()> {
        let chain_height = self.fuel_api.latest_height().await?;

        let Some(starting_height) = self.determine_starting_height(chain_height).await? else {
            info!("Database is up to date with the chain({chain_height}); no import necessary.");
            return Ok(());
        };

        self.fuel_api
            .full_blocks_in_height_range(starting_height..=chain_height)
            .map_err(crate::Error::from)
            .try_for_each(|blocks| async {
                self.validate_blocks(&blocks)?;

                self.import_blocks(blocks).await?;

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
    use ports::types::nonempty;
    use rand::{rngs::StdRng, SeedableRng};

    use super::*;
    use crate::{
        test_utils::{self, Blocks, ImportedBlocks},
        BlockValidator, Error,
    };

    #[tokio::test]
    async fn imports_first_block_when_db_is_empty() -> Result<()> {
        // Given
        let setup = test_utils::Setup::init().await;

        let mut rng = StdRng::from_seed([0; 32]);
        let secret_key = SecretKey::random(&mut rng);
        let block = test_utils::mocks::fuel::generate_block(0, &secret_key, 1, 100);

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

        let expected_block = encode_blocks(nonempty![block]);

        assert_eq!(all_blocks.into_inner(), expected_block);

        Ok(())
    }

    #[tokio::test]
    async fn wont_import_invalid_blocks() -> Result<()> {
        // Given
        let setup = test_utils::Setup::init().await;

        let mut rng = StdRng::from_seed([0; 32]);
        let correct_secret_key = SecretKey::random(&mut rng);
        let block_validator = BlockValidator::new(*correct_secret_key.public_key().hash());

        let incorrect_secret_key = SecretKey::random(&mut rng);
        let block = test_utils::mocks::fuel::generate_block(0, &incorrect_secret_key, 1, 100);

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(vec![block.clone()]);

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 0);

        // When
        let result = importer.run().await;

        // Then
        let Err(Error::BlockValidation(msg)) = result else {
            panic!("expected a validation error, got: {:?}", result);
        };

        assert_eq!(
            msg,
            r#"recovered producer addr `13d5eed3c6132bcf8dc2f92944d11fb3dc32df5ed183ab4716914eb21fd2b318` does not match expected addr`4747f47fb79e2b73b2f3c3ca1ea69d9b2b0caf8ac2d3480da6e750664f40914b`."#
        );

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
                range: 0..=2,
                tx_per_block: 1,
                size_per_tx: 100,
            })
            .await;

        let new_blocks = (3..=5)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key, 1, 100))
            .collect_vec();

        let all_blocks = existing_blocks
            .into_iter()
            .chain(new_blocks.clone())
            .collect_nonempty()
            .unwrap();

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

        let expected_blocks = encode_blocks(all_blocks);

        pretty_assertions::assert_eq!(stored_blocks.into_inner(), expected_blocks);

        Ok(())
    }

    #[tokio::test]
    async fn fails_if_db_height_is_greater_than_chain_height() -> Result<()> {
        // Given
        let setup = test_utils::Setup::init().await;

        let secret_key = setup
            .import_blocks(Blocks::WithHeights {
                range: 0..=5,
                tx_per_block: 1,
                size_per_tx: 100,
            })
            .await
            .secret_key;

        let chain_blocks = (0..=2)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key, 1, 100))
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
                range: 0..=2,
                tx_per_block: 1,
                size_per_tx: 100,
            })
            .await;

        let starting_height = 8;
        let new_blocks = (starting_height..=13)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key, 1, 100))
            .collect_nonempty()
            .unwrap();

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
        let expected_blocks = encode_blocks(new_blocks);

        pretty_assertions::assert_eq!(stored_new_blocks.into_inner(), expected_blocks);

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
                range: 0..=2,
                tx_per_block: 1,
                size_per_tx: 100,
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

        assert_eq!(stored_blocks.into_inner(), storage_blocks);

        Ok(())
    }
}
