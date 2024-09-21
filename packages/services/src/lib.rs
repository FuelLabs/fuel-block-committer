mod block_committer;
mod block_importer;
mod commit_listener;
mod health_reporter;
mod state_committer;
mod state_listener;
mod status_reporter;
mod wallet_balance_tracker;

pub use block_committer::BlockCommitter;
pub use block_importer::BlockImporter;
pub use commit_listener::CommitListener;
pub use health_reporter::HealthReporter;
pub use state_committer::{
    bundler::CompressionLevel, bundler::Factory as BundlerFactory, Config as StateCommitterConfig,
    StateCommitter,
};
pub use state_listener::StateListener;
pub use status_reporter::StatusReporter;
pub use wallet_balance_tracker::WalletBalanceTracker;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Other(String),
    #[error("Network error: {0}")]
    Network(String),
    #[error("Storage error: {0}")]
    Storage(String),
    #[error("Block validation error: {0}")]
    BlockValidation(String),
}

impl From<ports::l1::Error> for Error {
    fn from(error: ports::l1::Error) -> Self {
        match error {
            ports::l1::Error::Network(e) => Self::Network(e),
            _ => Self::Other(error.to_string()),
        }
    }
}

impl From<ports::fuel::Error> for Error {
    fn from(error: ports::fuel::Error) -> Self {
        match error {
            ports::fuel::Error::Network(e) => Self::Network(e),
            ports::fuel::Error::Other(e) => Self::Other(e.to_string()),
        }
    }
}

impl From<validator::Error> for Error {
    fn from(error: validator::Error) -> Self {
        match error {
            validator::Error::BlockValidation(e) => Self::BlockValidation(e),
        }
    }
}

impl From<ports::storage::Error> for Error {
    fn from(error: ports::storage::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[trait_variant::make(Send)]
pub trait Runner: Send + Sync {
    async fn run(&mut self) -> Result<()>;
}

#[cfg(test)]
pub(crate) mod test_utils {

    pub async fn encode_and_merge(
        blocks: impl IntoIterator<Item = ports::fuel::FullFuelBlock>,
    ) -> NonEmptyVec<u8> {
        let blocks = blocks.into_iter().collect::<Vec<_>>();

        if blocks.is_empty() {
            panic!("blocks must not be empty");
        }

        let blocks = NonEmptyVec::try_from(blocks).expect("is not empty");
               

        let bytes = block_importer::encode_blocks(blocks).into_iter().flat_map(|b|b.data).collect_vec();

        bytes.try_into().expect("is not empty")
    }

    pub fn random_data(size: impl Into<usize>) -> NonEmptyVec<u8> {
        let size = size.into();
        if size == 0 {
            panic!("random data size must be greater than 0");
        }

        // TODO: segfault use better random data generation
        let data: Vec<u8> = (0..size).map(|_| rand::random::<u8>()).collect();

        data.try_into().expect("is not empty due to check")
    }

    use std::{ops::{Range, RangeInclusive}, sync::Arc};

    use clock::TestClock;
    use eth::Eip4844GasUsage;
    use fuel_crypto::SecretKey;
    use itertools::Itertools;
    use mocks::l1::TxStatus;
    use ports::types::{DateTime, NonEmptyVec, Utc};
    use storage::{DbWithProcess, PostgresProcess};
    use validator::BlockValidator;

    use crate::{
        block_importer::{self, encode_blocks},
        state_committer::bundler::{self},
        BlockImporter, StateCommitter, StateCommitterConfig, StateListener,
    };

    use super::Runner;

    pub mod mocks {
        pub mod l1 {
            use std::num::NonZeroUsize;

            use delegate::delegate;
            use mockall::{predicate::eq, Sequence};
            use ports::{
                l1::{Api, GasPrices},
                types::{L1Height, NonEmptyVec, TransactionResponse, U256},
            };

            pub struct FullL1Mock {
                pub api: ports::l1::MockApi,
                pub contract: ports::l1::MockContract,
            }

            impl Default for FullL1Mock {
                fn default() -> Self {
                    Self::new()
                }
            }

            impl FullL1Mock {
                pub fn new() -> Self {
                    let mut obj = Self {
                        api: ports::l1::MockApi::new(),
                        contract: ports::l1::MockContract::new(),
                    };

                    obj.api.expect_gas_prices().returning(|| {
                        Box::pin(async {
                            Ok(GasPrices {
                                storage: 10,
                                normal: 1,
                            })
                        })
                    });

                    obj
                }
            }

            impl ports::l1::Contract for FullL1Mock {
                delegate! {
                    to self.contract {
                        async fn submit(&self, block: ports::types::ValidatedFuelBlock) -> ports::l1::Result<()>;
                        fn event_streamer(&self, height: L1Height) -> Box<dyn ports::l1::EventStreamer + Send + Sync>;
                        fn commit_interval(&self) -> std::num::NonZeroU32;
                    }
                }
            }

            impl ports::l1::Api for FullL1Mock {
                delegate! {
                    to self.api {
                        async fn gas_prices(&self) -> ports::l1::Result<GasPrices>;
                        async fn submit_l2_state(&self, state_data: NonEmptyVec<u8>) -> ports::l1::Result<[u8; 32]>;
                        async fn get_block_number(&self) -> ports::l1::Result<L1Height>;
                        async fn balance(&self) -> ports::l1::Result<U256>;
                        async fn get_transaction_response(&self, tx_hash: [u8; 32]) -> ports::l1::Result<Option<TransactionResponse>>;
                    }
                }
            }

            pub enum TxStatus {
                Success,
                Failure,
            }

            pub fn expects_state_submissions(
                expectations: impl IntoIterator<Item = (Option<NonEmptyVec<u8>>, [u8; 32])>,
            ) -> ports::l1::MockApi {
                let mut sequence = Sequence::new();

                let mut l1_mock = ports::l1::MockApi::new();
                l1_mock.expect_gas_prices().returning(|| {
                    Box::pin(async {
                        Ok(GasPrices {
                            storage: 10,
                            normal: 1,
                        })
                    })
                });

                for (fragment, tx_id) in expectations {
                    l1_mock
                        .expect_submit_l2_state()
                        .withf(move |data| {
                            if let Some(fragment) = &fragment {
                                data == fragment
                            } else {
                                true
                            }
                        })
                        .once()
                        .return_once(move |_| Box::pin(async move { Ok(tx_id) }))
                        .in_sequence(&mut sequence);
                }

                l1_mock
            }

            pub fn txs_finished(
                statuses: impl IntoIterator<Item = ([u8; 32], TxStatus)>,
            ) -> ports::l1::MockApi {
                let mut l1_mock = ports::l1::MockApi::new();

                let height = L1Height::from(0);
                l1_mock
                    .expect_get_block_number()
                    .returning(move || Box::pin(async move { Ok(height) }));

                for expectation in statuses {
                    let (tx_id, status) = expectation;

                    l1_mock
                        .expect_get_transaction_response()
                        .with(eq(tx_id))
                        .return_once(move |_| {
                            Box::pin(async move {
                                Ok(Some(TransactionResponse::new(
                                    height.into(),
                                    matches!(status, TxStatus::Success),
                                )))
                            })
                        });
                }
                l1_mock
            }
        }

        pub mod fuel {

            use std::{
                iter,
                ops::{Range, RangeInclusive},
            };

            use fuel_crypto::{Message, SecretKey, Signature};
            use futures::{stream, StreamExt};
            use itertools::Itertools;
            use ports::{
                fuel::{FuelBlock, FuelBlockId, FuelConsensus, FuelHeader, FuelPoAConsensus, FullFuelBlock}, non_empty_vec, storage::SequentialFuelBlocks, types::NonEmptyVec
            };
            use rand::{Rng, RngCore, SeedableRng};

            use crate::block_importer;

            pub fn generate_block(
                height: u32,
                secret_key: &SecretKey,
                num_tx: usize,
                tx_size: usize
            ) -> ports::fuel::FullFuelBlock {
                let header = given_header(height);

                let mut hasher = fuel_crypto::Hasher::default();
                hasher.input(header.prev_root.as_ref());
                hasher.input(header.height.to_be_bytes());
                hasher.input(header.time.0.to_be_bytes());
                hasher.input(header.application_hash.as_ref());

                let id = FuelBlockId::from(hasher.digest());
                let id_message = Message::from_bytes(*id);
                let signature = Signature::sign(secret_key, &id_message);

                let mut small_rng = rand::rngs::SmallRng::from_seed([0; 32]);
                let raw_transactions = std::iter::repeat_with(|| {
                    let mut buf = vec![0; tx_size];
                    small_rng.fill_bytes(&mut buf);
                    NonEmptyVec::try_from(buf).unwrap()
                })
                    .take(num_tx)
                    .collect::<Vec<_>>();

                FullFuelBlock {
                    id,
                    header,
                    consensus: FuelConsensus::PoAConsensus(FuelPoAConsensus { signature }),
                    raw_transactions,
                }
            }

            pub fn generate_storage_block_sequence(
                heights: RangeInclusive<u32>,
                secret_key: &SecretKey,
                num_tx: usize,
                tx_size: usize
            ) -> SequentialFuelBlocks {
                let blocks = heights
                    .map(|height| generate_storage_block(height, secret_key, num_tx, tx_size))
                    .collect_vec();

                let non_empty_blocks =
                    NonEmptyVec::try_from(blocks).expect("test gave an invalid range");

                non_empty_blocks
                    .try_into()
                    .expect("generated from a range, guaranteed sequence of heights")
            }

            pub fn generate_storage_block(
                height: u32,
                secret_key: &SecretKey,
                num_tx: usize,
                tx_size: usize
            ) -> ports::storage::FuelBlock {
                let block = generate_block(height, secret_key, num_tx, tx_size);
                block_importer::encode_blocks(non_empty_vec![block]).take_first()
            }

            fn given_header(height: u32) -> FuelHeader {
                let application_hash =
                    "0x8b96f712e293e801d53da77113fec3676c01669c6ea05c6c92a5889fce5f649d"
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

            pub fn blocks_exists( 
                secret_key: SecretKey,
                heights: Range<u32>,
            ) -> ports::fuel::MockApi {
                let blocks = heights
                    .map(|height| generate_block(height, &secret_key, 1, 100))
                    .collect::<Vec<_>>();

                these_blocks_exist(blocks)
            }

            pub fn these_blocks_exist(
                blocks: impl IntoIterator<Item = ports::fuel::FullFuelBlock>,
            ) -> ports::fuel::MockApi {
                let mut fuel_mock = ports::fuel::MockApi::default();

                let blocks = blocks
                    .into_iter()
                    .sorted_by_key(|b| b.header.height)
                    .collect::<Vec<_>>();

                let latest_block = blocks.last().expect("Must have at least one block").clone();

                let lowest_height = blocks
                    .first()
                    .expect("Must have at least one block")
                    .header
                    .height;
                let highest_height = latest_block.header.height;

                fuel_mock
                    .expect_latest_height()
                    .return_once(move || Box::pin(async move { Ok(highest_height) }));

                fuel_mock
                    .expect_full_blocks_in_height_range()
                    .returning(move |range| {
                        let expected_range = lowest_height..=highest_height;
                        if range != expected_range {
                            panic!("range of requested blocks {range:?} is not as tight as expected: {expected_range:?}");
                        }

                        let blocks_batch = blocks
                            .iter()
                            .filter(move |b| range.contains(&b.header.height))
                            .cloned()
                            .collect_vec().try_into().expect("is not empty");
                                                    
                        
                        stream::iter(iter::once(Ok(blocks_batch))).boxed()
                    });

                fuel_mock
            }
        }
    }

    #[derive(Debug)]
    pub struct ImportedBlocks {
        pub fuel_blocks: NonEmptyVec<ports::fuel::FullFuelBlock>,
        pub storage_blocks: NonEmptyVec<ports::storage::FuelBlock>,
        pub secret_key: SecretKey,
    }

    pub struct Setup {
        db: DbWithProcess,
    }

    impl Setup {
        pub async fn init() -> Self {
            let db = PostgresProcess::shared()
                .await
                .unwrap()
                .create_random_db()
                .await
                .unwrap();
            Self { db }
        }

        pub fn db(&self) -> DbWithProcess {
            self.db.clone()
        }

        pub async fn commit_single_block_bundle(&self, finalization_time: DateTime<Utc>) {
            self.import_blocks(Blocks::WithHeights {
                range: 0..=0,
                tx_per_block: 1,
                size_per_tx: 100
            })
            .await;

            let clock = TestClock::default();
            clock.set_time(finalization_time);

            let factory = bundler::Factory::new(Eip4844GasUsage, crate::CompressionLevel::Level6);

            let tx = [2u8; 32];

            let l1_mock = mocks::l1::expects_state_submissions(vec![(None, tx)]);
            let mut committer = StateCommitter::new(
                l1_mock,
                self.db(),
                clock.clone(),
                factory,
                StateCommitterConfig::default(),
            );
            committer.run().await.unwrap();

            let l1_mock = mocks::l1::txs_finished([(tx, TxStatus::Success)]);

            StateListener::new(l1_mock, self.db(), 0, clock.clone())
                .run()
                .await
                .unwrap();
        }

        pub async fn import_blocks(&self, blocks: Blocks) -> ImportedBlocks {
            let (mut block_importer, blocks) = self.block_importer(blocks);

            block_importer.run().await.unwrap();

            blocks
        }

        pub async fn report_txs_finished(
            &self,
            statuses: impl IntoIterator<Item = ([u8; 32], TxStatus)>,
        ) {
            let l1_mock = mocks::l1::txs_finished(statuses);

            StateListener::new(l1_mock, self.db(), 0, TestClock::default())
                .run()
                .await
                .unwrap()
        }

        pub fn block_importer(
            &self,
            blocks: Blocks,
        ) -> (
            BlockImporter<DbWithProcess, ports::fuel::MockApi, BlockValidator>,
            ImportedBlocks,
        ) {
            match blocks {
                Blocks::WithHeights {
                    range,
                    tx_per_block,
                    size_per_tx,
                } => {
                    let secret_key = SecretKey::random(&mut rand::thread_rng());

                    let block_validator = BlockValidator::new(*secret_key.public_key().hash());

                    let blocks = range
                        .map(|height| {
                            mocks::fuel::generate_block(height, &secret_key, tx_per_block, size_per_tx)
                        })
                        .collect::<Vec<_>>();

                    let storage_blocks = encode_blocks(blocks.clone().try_into().unwrap());

                    let mock = mocks::fuel::these_blocks_exist(blocks.clone());

                    (
                        BlockImporter::new(self.db(), mock, block_validator, 0),
                        ImportedBlocks {
                            fuel_blocks: blocks.try_into().unwrap(),
                            secret_key,
                            storage_blocks,
                        },
                    )
                }
                Blocks::Blocks { blocks, secret_key } => {
                    let block_validator = BlockValidator::new(*secret_key.public_key().hash());
                    let mock = mocks::fuel::these_blocks_exist(blocks.clone());

                    let storage_blocks = block_importer::encode_blocks(blocks.clone().try_into().unwrap());
                    (
                        BlockImporter::new(self.db(), mock, block_validator, 0),
                        ImportedBlocks {
                            fuel_blocks: blocks,
                            storage_blocks,
                            secret_key,
                        },
                    )
                }
            }
        }
    }

    pub enum Blocks {
        WithHeights {
            range: RangeInclusive<u32>,
            tx_per_block: usize,
            size_per_tx: usize,
        },
        Blocks {
            blocks: NonEmptyVec<ports::fuel::FullFuelBlock>,
            secret_key: SecretKey,
        },
    }

    impl Blocks {
        pub fn len(&self) -> usize {
            match self {
                Self::WithHeights { range, .. } => range.clone().count(),
                Self::Blocks { blocks, .. } => blocks.len().get(),
            }
        }
    }
}
