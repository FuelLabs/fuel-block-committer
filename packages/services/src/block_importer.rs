use crate::{validator::Validator, Result, Runner};
use futures::TryStreamExt;
use itertools::chain;
use ports::fuel::MaybeCompressedFuelBlock;
use ports::{
    fuel::{Consensus, FuelPoAConsensus, FullFuelBlock, Genesis},
    storage::Storage,
    types::{CollectNonEmpty, NonEmpty},
};
use tracing::info;

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
    async fn import_blocks(&self, blocks: NonEmpty<MaybeCompressedFuelBlock>) -> Result<()> {
        let db_blocks = encode_blocks(blocks);

        let starting_height = db_blocks.first().height();
        let ending_height = db_blocks.last().height();

        self.storage.insert_blocks(db_blocks).await?;

        info!("Imported blocks: {starting_height}..={ending_height}");

        Ok(())
    }

    fn validate_blocks(&self, blocks: &[MaybeCompressedFuelBlock]) -> Result<()> {
        for block in blocks {
            match block {
                MaybeCompressedFuelBlock::Uncompressed(block) => {
                    self.block_validator
                        .validate(block.id, &block.header, &block.consensus)?;
                }
                MaybeCompressedFuelBlock::Compressed(_) => {
                    // We don't validate compressed blocks
                }
            }
        }

        Ok(())
    }
}

pub(crate) fn encode_blocks(
    blocks: NonEmpty<MaybeCompressedFuelBlock>,
) -> NonEmpty<ports::storage::SerializedFuelBlock> {
    blocks
        .into_iter()
        .map(|full_block| match full_block {
            MaybeCompressedFuelBlock::Compressed(compressed) => {
                ports::storage::SerializedFuelBlock::Compressed(compressed)
            }
            MaybeCompressedFuelBlock::Uncompressed(block) => {
                ports::storage::SerializedFuelBlock::Uncompressed(ports::storage::FuelBlock {
                    hash: *block.id,
                    height: block.header.height,
                    data: encode_block_data(block),
                })
            }
        })
        .collect_nonempty()
        .expect("should be non-empty")
}

fn serialize_header(header: ports::fuel::FuelHeader) -> NonEmpty<u8> {
    chain!(
        *header.id,
        header.da_height.to_be_bytes(),
        header.consensus_parameters_version.to_be_bytes(),
        header.state_transition_bytecode_version.to_be_bytes(),
        header.transactions_count.to_be_bytes(),
        header.message_receipt_count.to_be_bytes(),
        *header.transactions_root,
        *header.message_outbox_root,
        *header.event_inbox_root,
        header.height.to_be_bytes(),
        *header.prev_root,
        header.time.0.to_be_bytes(),
        *header.application_hash,
    )
    .collect_nonempty()
    .expect("should be non-empty")
}

fn serialize_consensus(consensus: Consensus) -> NonEmpty<u8> {
    let mut buf = vec![];
    match consensus {
        Consensus::Genesis(Genesis {
            chain_config_hash,
            coins_root,
            contracts_root,
            messages_root,
            transactions_root,
        }) => {
            let variant = 0u8;
            buf.extend(chain!(
                variant.to_be_bytes(),
                *chain_config_hash,
                *coins_root,
                *contracts_root,
                *messages_root,
                *transactions_root,
            ));
        }
        Consensus::PoAConsensus(FuelPoAConsensus { signature }) => {
            let variant = 1u8;

            buf.extend(chain!(variant.to_be_bytes(), *signature));
        }
        Consensus::Unknown => {
            let variant = 2u8;
            buf.extend(variant.to_be_bytes());
        }
    }

    NonEmpty::from_vec(buf).expect("should be non-empty")
}

fn encode_block_data(block: FullFuelBlock) -> NonEmpty<u8> {
    // We don't handle fwd/bwd compatibility, that should be handled once the DA compression on the core is incorporated
    chain!(
        serialize_header(block.header),
        serialize_consensus(block.consensus),
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
    async fn run(&mut self) -> Result<()> {
        let chain_height = self.fuel_api.latest_height().await?;
        let starting_height = chain_height.saturating_sub(self.lookback_window);

        for range in self
            .storage
            .missing_blocks(starting_height, chain_height)
            .await?
        {
            self.fuel_api
                .full_blocks_in_height_range(range)
                .map_err(crate::Error::from)
                .try_for_each(|blocks| async {
                    self.validate_blocks(&blocks)?;

                    if let Some(blocks) = NonEmpty::from_vec(blocks) {
                        self.import_blocks(blocks).await?;
                    }

                    Ok(())
                })
                .await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{
        test_utils::{self, Blocks, ImportedBlocks},
        BlockValidator, Error,
    };
    use fuel_crypto::SecretKey;
    use futures::StreamExt;
    use itertools::Itertools;
    use mockall::{predicate::eq, Sequence};
    use ports::types::nonempty;
    use rand::{rngs::StdRng, SeedableRng};

    #[tokio::test]
    async fn imports_first_block_when_db_is_empty() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let mut rng = StdRng::from_seed([0; 32]);
        let secret_key = SecretKey::random(&mut rng);
        let block = test_utils::mocks::fuel::generate_block(0, &secret_key, 1, 100);

        let fuel_mock =
            test_utils::mocks::fuel::these_blocks_exist(vec![block.clone().into()], true);
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 0);

        // when
        importer.run().await?;

        // then
        let all_blocks = setup
            .db()
            .lowest_sequence_of_unbundled_blocks(0, 10)
            .await?
            .unwrap();

        let expected_block =
            encode_blocks(nonempty![MaybeCompressedFuelBlock::Uncompressed(block)])
                .try_into()
                .unwrap();

        assert_eq!(all_blocks, expected_block);

        Ok(())
    }

    #[tokio::test]
    async fn wont_import_invalid_blocks() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let mut rng = StdRng::from_seed([0; 32]);
        let correct_secret_key = SecretKey::random(&mut rng);
        let block_validator = BlockValidator::new(*correct_secret_key.public_key().hash());

        let incorrect_secret_key = SecretKey::random(&mut rng);
        let block = test_utils::mocks::fuel::generate_block(0, &incorrect_secret_key, 1, 100);

        let fuel_mock =
            test_utils::mocks::fuel::these_blocks_exist(vec![block.clone().into()], true);

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 0);

        // when
        let result = importer.run().await;

        // then
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
        // given
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
            .map(MaybeCompressedFuelBlock::from)
            .collect_vec();

        let all_blocks = existing_blocks
            .into_iter()
            .chain(new_blocks.clone())
            .collect_nonempty()
            .unwrap();

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(new_blocks.clone(), true);
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 1000);

        // when
        importer.run().await?;

        // then
        let stored_blocks = setup
            .db()
            .lowest_sequence_of_unbundled_blocks(0, 100)
            .await?
            .unwrap();

        let expected_blocks = encode_blocks(all_blocks).try_into().unwrap();

        pretty_assertions::assert_eq!(stored_blocks, expected_blocks);

        Ok(())
    }

    #[tokio::test]
    async fn respects_height_even_if_blocks_before_are_missing() -> Result<()> {
        // given
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
            .map(MaybeCompressedFuelBlock::from)
            .collect_nonempty()
            .unwrap();

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(new_blocks.clone(), true);
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 5);

        // when
        importer.run().await?;

        // then
        let stored_new_blocks = setup
            .db()
            .lowest_sequence_of_unbundled_blocks(starting_height, 100)
            .await?
            .unwrap();
        let expected_blocks = encode_blocks(new_blocks);

        pretty_assertions::assert_eq!(stored_new_blocks, expected_blocks.try_into().unwrap());

        Ok(())
    }

    #[tokio::test]
    async fn handles_chain_with_no_new_blocks() -> Result<()> {
        // given
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

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(fuel_blocks, true);
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 0);

        // when
        importer.run().await?;

        // then
        // Database should remain unchanged
        let stored_blocks = setup
            .db()
            .lowest_sequence_of_unbundled_blocks(0, 10)
            .await?
            .unwrap();

        assert_eq!(stored_blocks.into_inner(), storage_blocks);

        Ok(())
    }

    #[tokio::test]
    async fn skips_blocks_outside_lookback_window() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;
        let lookback_window = 2;

        let secret_key = SecretKey::random(&mut StdRng::from_seed([0; 32]));
        let blocks_to_import = (3..=5).map(|height| {
            test_utils::mocks::fuel::generate_block(height, &secret_key, 1, 100).into()
        });

        let fuel_mock = test_utils::mocks::fuel::these_blocks_exist(blocks_to_import, true);
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer =
            BlockImporter::new(setup.db(), fuel_mock, block_validator, lookback_window);

        // when
        importer.run().await?;

        // then
        let unbundled_blocks = setup
            .db()
            .lowest_sequence_of_unbundled_blocks(0, 10)
            .await?
            .unwrap();

        let unbundled_block_heights: Vec<_> = unbundled_blocks
            .into_inner()
            .iter()
            .map(|b| b.height)
            .collect();

        assert_eq!(
            unbundled_block_heights,
            vec![3, 4, 5],
            "Blocks outside the lookback window should remain unbundled"
        );

        Ok(())
    }

    #[tokio::test]
    async fn fills_in_missing_blocks_inside_lookback_window() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let secret_key = SecretKey::random(&mut StdRng::from_seed([0; 32]));

        for range in [(3..=10), (40..=60)] {
            setup
                .import_blocks(Blocks::WithHeights {
                    range,
                    tx_per_block: 1,
                    size_per_tx: 100,
                })
                .await;
        }

        let mut fuel_mock = ports::fuel::MockApi::new();

        let mut sequence = Sequence::new();

        for range in [0..=2, 11..=39, 61..=100] {
            fuel_mock
                .expect_full_blocks_in_height_range()
                .with(eq(range))
                .once()
                .in_sequence(&mut sequence)
                .return_once(move |range| {
                    let blocks = range
                        .map(|height| {
                            test_utils::mocks::fuel::generate_block(height, &secret_key, 1, 100)
                        })
                        .map(MaybeCompressedFuelBlock::from)
                        .collect();

                    futures::stream::once(async { Ok(blocks) }).boxed()
                });
        }

        fuel_mock
            .expect_latest_height()
            .once()
            .return_once(|| Box::pin(async { Ok(100) }));

        let block_validator = BlockValidator::new(*secret_key.public_key().hash());

        let mut importer = BlockImporter::new(setup.db(), fuel_mock, block_validator, 101);

        // when
        importer.run().await?;

        // then
        let unbundled_blocks = setup
            .db()
            .lowest_sequence_of_unbundled_blocks(0, 101)
            .await?
            .unwrap();

        let unbundled_block_heights: Vec<_> = unbundled_blocks
            .into_inner()
            .iter()
            .map(|b| b.height)
            .collect();

        assert_eq!(
            unbundled_block_heights,
            (0..=100).collect_vec(),
            "Blocks outside the lookback window should remain unbundled"
        );

        Ok(())
    }
}
