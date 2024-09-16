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
pub use state_committer::StateCommitter;
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

#[async_trait::async_trait]
pub trait Runner: Send + Sync {
    async fn run(&mut self) -> Result<()>;
}

#[cfg(test)]
pub(crate) mod test_utils {
    pub(crate) async fn merge_and_compress_blocks(
        blocks: &[ports::storage::FuelBlock],
    ) -> NonEmptyVec<u8> {
        let compressor = Compressor::default();
        let merged_bytes: Vec<_> = blocks
            .iter()
            .flat_map(|b| b.data.inner())
            .copied()
            .collect();

        let merged_bytes: NonEmptyVec<u8> = merged_bytes
            .try_into()
            .expect("Merged data cannot be empty");

        compressor.compress(&merged_bytes).await.unwrap()
    }

    pub async fn encode_merge_and_compress_blocks<'a>(
        blocks: impl IntoIterator<Item = &'a ports::fuel::FuelBlock>,
    ) -> NonEmptyVec<u8> {
        let blocks = blocks.into_iter().collect::<Vec<_>>();

        if blocks.is_empty() {
            panic!("blocks must not be empty");
        }

        let bytes: Vec<u8> = blocks
            .into_iter()
            .flat_map(|block| {
                block_importer::encode_block_data(block)
                    .unwrap()
                    .into_inner()
            })
            .collect();

        Compressor::default()
            .compress(&bytes.try_into().expect("is not empty"))
            .await
            .unwrap()
    }

    pub fn random_data(size: usize) -> NonEmptyVec<u8> {
        if size == 0 {
            panic!("random data size must be greater than 0");
        }

        // TODO: segfault use better random data generation
        let data: Vec<u8> = (0..size).map(|_| rand::random::<u8>()).collect();

        data.try_into().expect("is not empty due to check")
    }

    use std::{ops::Range, sync::Arc};

    use clock::TestClock;
    use fuel_crypto::SecretKey;
    use mocks::l1::TxStatus;
    use ports::types::NonEmptyVec;
    use storage::PostgresProcess;
    use validator::BlockValidator;

    use crate::{
        block_importer, state_committer::bundler::Compressor, BlockImporter, StateListener,
    };

    use super::Runner;

    pub mod mocks {
        pub mod l1 {
            use mockall::{predicate::eq, Sequence};
            use ports::{
                l1::SubmittableFragments,
                types::{L1Height, NonEmptyVec, TransactionResponse},
            };

            pub enum TxStatus {
                Success,
                Failure,
            }

            pub fn expects_state_submissions(
                expectations: impl IntoIterator<Item = (NonEmptyVec<u8>, [u8; 32])>,
            ) -> ports::l1::MockApi {
                let mut sequence = Sequence::new();

                let mut l1_mock = ports::l1::MockApi::new();
                for (fragment, tx_id) in expectations {
                    l1_mock
                        .expect_submit_l2_state()
                        .with(eq(fragment))
                        .once()
                        .return_once(move |_| Ok(tx_id))
                        .in_sequence(&mut sequence);
                }

                l1_mock
            }

            pub fn will_split_bundle_into_fragments(
                fragments: SubmittableFragments,
            ) -> ports::l1::MockApi {
                let mut l1_mock = ports::l1::MockApi::new();

                l1_mock
                    .expect_split_into_submittable_fragments()
                    .once()
                    .return_once(move |_| Ok(fragments));

                l1_mock
            }
            pub fn will_split_bundles_into_fragments(
                expectations: impl IntoIterator<Item = (NonEmptyVec<u8>, SubmittableFragments)>,
            ) -> ports::l1::MockApi {
                let mut l1_mock = ports::l1::MockApi::new();
                let mut sequence = Sequence::new();
                for (bundle, fragments) in expectations {
                    l1_mock
                        .expect_split_into_submittable_fragments()
                        .with(eq(bundle))
                        .once()
                        .return_once(move |_| Ok(fragments))
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
                    .returning(move || Ok(height));

                for expectation in statuses {
                    let (tx_id, status) = expectation;

                    l1_mock
                        .expect_get_transaction_response()
                        .with(eq(tx_id))
                        .return_once(move |_| {
                            Ok(Some(TransactionResponse::new(
                                height.into(),
                                matches!(status, TxStatus::Success),
                            )))
                        });
                }
                l1_mock
            }
        }

        pub mod fuel {

            use std::ops::Range;

            use fuel_crypto::{Message, SecretKey, Signature};
            use futures::{stream, StreamExt};
            use itertools::Itertools;
            use ports::fuel::{
                FuelBlock, FuelBlockId, FuelConsensus, FuelHeader, FuelPoAConsensus,
            };

            use crate::block_importer;

            pub fn generate_block(height: u32, secret_key: &SecretKey) -> ports::fuel::FuelBlock {
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

            pub fn generate_storage_block(
                height: u32,
                secret_key: &SecretKey,
            ) -> ports::storage::FuelBlock {
                let block = generate_block(height, secret_key);
                ports::storage::FuelBlock {
                    hash: *block.id,
                    height: block.header.height,
                    data: block_importer::encode_block_data(&block).unwrap(),
                }
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
                    .map(|height| generate_block(height, &secret_key))
                    .collect::<Vec<_>>();

                these_blocks_exist(blocks)
            }

            pub fn these_blocks_exist(
                blocks: impl IntoIterator<Item = ports::fuel::FuelBlock>,
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
                    .expect_latest_block()
                    .return_once(|| Ok(latest_block));

                fuel_mock
                    .expect_blocks_in_height_range()
                    .returning(move |range| {
                        if let Some(lowest) = range.clone().min() {
                            if lowest < lowest_height {
                                panic!("The range of blocks asked of the mock is not tight!");
                            }
                        }

                        if let Some(highest) = range.clone().max() {
                            if highest > highest_height {
                                panic!("The range of blocks asked of the mock is not tight!");
                            }
                        }

                        let blocks = blocks
                            .iter()
                            .filter(move |b| range.contains(&b.header.height))
                            .cloned()
                            .map(Ok)
                            .collect_vec();
                        stream::iter(blocks).boxed()
                    });

                fuel_mock
            }
        }
    }

    pub struct Setup {
        _db_process: Arc<PostgresProcess>,
        db: storage::Postgres,
    }

    impl Setup {
        pub async fn init() -> Self {
            let db_process = PostgresProcess::shared().await.unwrap();
            let db = db_process.create_random_db().await.unwrap();
            Self {
                _db_process: db_process,
                db,
            }
        }

        pub fn db(&self) -> storage::Postgres {
            self.db.clone()
        }

        pub async fn import_blocks(&self, blocks: Blocks) {
            self.block_importer(blocks).run().await.unwrap()
        }

        pub async fn report_txs_finished(
            &self,
            statuses: impl IntoIterator<Item = ([u8; 32], TxStatus)>,
        ) {
            let l1_mock = mocks::l1::txs_finished(statuses);

            StateListener::new(Arc::new(l1_mock), self.db(), 0, TestClock::default())
                .run()
                .await
                .unwrap()
        }

        pub fn block_importer(
            &self,
            blocks: Blocks,
        ) -> BlockImporter<storage::Postgres, ports::fuel::MockApi, BlockValidator> {
            let amount = blocks.len();

            match blocks {
                Blocks::WithHeights(range) => {
                    let secret_key = SecretKey::random(&mut rand::thread_rng());

                    let block_validator = BlockValidator::new(*secret_key.public_key().hash());
                    let mock = mocks::fuel::blocks_exists(secret_key, range);

                    BlockImporter::new(self.db(), mock, block_validator, amount as u32)
                }
                Blocks::Blocks { blocks, secret_key } => {
                    let block_validator = BlockValidator::new(*secret_key.public_key().hash());
                    let mock = mocks::fuel::these_blocks_exist(blocks);

                    BlockImporter::new(self.db(), mock, block_validator, amount as u32)
                }
            }
        }
    }

    pub enum Blocks {
        WithHeights(Range<u32>),
        Blocks {
            blocks: Vec<ports::fuel::FuelBlock>,
            secret_key: SecretKey,
        },
    }

    impl Blocks {
        pub fn len(&self) -> usize {
            match self {
                Self::WithHeights(range) => range.len(),
                Self::Blocks { blocks, .. } => blocks.len(),
            }
        }
    }
}
