mod block_committer;
mod commit_listener;
mod health_reporter;
mod state_committer;
mod state_importer;
mod state_listener;
mod status_reporter;
mod wallet_balance_tracker;

pub use block_committer::BlockCommitter;
pub use commit_listener::CommitListener;
pub use health_reporter::HealthReporter;
pub use state_committer::StateCommitter;
pub use state_importer::StateImporter;
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
    use std::{ops::Range, sync::Arc};

    use clock::TestClock;
    use fuel_crypto::SecretKey;
    use mocks::l1::TxStatus;
    use storage::PostgresProcess;
    use validator::BlockValidator;

    use crate::{StateImporter, StateListener};

    use super::Runner;

    pub mod mocks {
        pub mod l1 {
            use mockall::predicate::eq;
            use ports::types::{L1Height, TransactionResponse};

            pub enum TxStatus {
                Success,
                Failure,
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

                fuel_mock
                    .expect_latest_block()
                    .return_once(|| Ok(latest_block));

                fuel_mock
                    .expect_blocks_in_height_range()
                    .returning(move |arg| {
                        let blocks = blocks
                            .iter()
                            .filter(move |b| arg.contains(&b.header.height))
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
            self.importer_of_blocks(blocks).run().await.unwrap()
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

        pub fn importer_of_blocks(
            &self,
            blocks: Blocks,
        ) -> StateImporter<storage::Postgres, ports::fuel::MockApi, BlockValidator> {
            let amount = blocks.len();

            match blocks {
                Blocks::WithHeights(range) => {
                    let secret_key = SecretKey::random(&mut rand::thread_rng());

                    let block_validator = BlockValidator::new(*secret_key.public_key().hash());
                    let mock = mocks::fuel::blocks_exists(secret_key, range);

                    StateImporter::new(self.db(), mock, block_validator, amount as u32)
                }
                Blocks::Blocks { blocks, secret_key } => {
                    let block_validator = BlockValidator::new(*secret_key.public_key().hash());
                    let mock = mocks::fuel::these_blocks_exist(blocks);

                    StateImporter::new(self.db(), mock, block_validator, amount as u32)
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
