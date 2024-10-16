mod block_bundler;
mod block_committer;
mod block_importer;
mod health_reporter;
mod state_committer;
mod state_listener;
mod status_reporter;
mod wallet_balance_tracker;

pub use block_bundler::{
    bundler::Factory as BundlerFactory, BlockBundler, Config as BlockBundlerConfig,
};
pub use block_committer::BlockCommitter;
pub use block_importer::BlockImporter;
pub use health_reporter::HealthReporter;
pub use state_committer::{Config as StateCommitterConfig, StateCommitter};
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

impl From<ports::storage::Error> for Error {
    fn from(error: ports::storage::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<String> for Error {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[trait_variant::make(Send)]
pub trait Runner: Send + Sync {
    async fn run(&mut self) -> Result<()>;
}

#[cfg(test)]
pub(crate) mod test_utils {
    use std::{ops::RangeInclusive, time::Duration};

    use clock::TestClock;
    use eth::BlobEncoder;
    use metrics::prometheus::IntGauge;
    use mocks::l1::TxStatus;
    use ports::{
        l1::FragmentEncoder,
        storage::Storage,
        types::{CollectNonEmpty, CompressedFuelBlock, DateTime, Fragment, NonEmpty, Utc},
    };
    use rand::RngCore;
    use storage::{DbWithProcess, PostgresProcess};
    use utils::bundle::{self, CompressionLevel};

    use super::Runner;
    use crate::{
        block_bundler::bundler::Factory, BlockBundler, BlockBundlerConfig, BlockImporter,
        StateCommitter, StateListener,
    };

    pub(crate) fn bundle_and_encode_into_blobs(
        blocks: NonEmpty<CompressedFuelBlock>,
        id: u16,
    ) -> NonEmpty<Fragment> {
        let blocks = blocks
            .into_iter()
            .map(|b| Vec::from(b.data))
            .collect::<Vec<_>>();

        let bundle = bundle::Bundle::V1(bundle::BundleV1 { blocks });

        let encoded_bundle = NonEmpty::from_vec(
            bundle::Encoder::new(CompressionLevel::Disabled)
                .encode(bundle)
                .unwrap(),
        )
        .unwrap();

        BlobEncoder.encode(encoded_bundle, id.into()).unwrap()
    }
    pub fn random_data(size: impl Into<usize>) -> NonEmpty<u8> {
        let size = size.into();
        if size == 0 {
            panic!("random data size must be greater than 0");
        }

        let mut buffer = vec![0; size];
        rand::thread_rng().fill_bytes(&mut buffer[..]);
        NonEmpty::collect(buffer).expect("checked size, not empty")
    }

    pub mod mocks {
        pub mod l1 {

            use std::cmp::min;

            use delegate::delegate;
            use mockall::{predicate::eq, Sequence};
            use ports::{
                l1::FragmentsSubmitted,
                types::{
                    BlockSubmissionTx, Fragment, L1Height, NonEmpty, TransactionResponse, U256,
                },
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
                    Self {
                        api: ports::l1::MockApi::new(),
                        contract: ports::l1::MockContract::new(),
                    }
                }
            }

            impl ports::l1::Contract for FullL1Mock {
                delegate! {
                    to self.contract {
                        async fn submit(&self, hash: [u8;32], height: u32) -> ports::l1::Result<BlockSubmissionTx>;
                        fn commit_interval(&self) -> std::num::NonZeroU32;
                    }
                }
            }

            impl ports::l1::Api for FullL1Mock {
                delegate! {
                    to self.api {
                        async fn submit_state_fragments(
                            &self,
                            fragments: NonEmpty<Fragment>,
                            previous_tx: Option<ports::types::L1Tx>,
                        ) -> ports::l1::Result<(ports::types::L1Tx, FragmentsSubmitted)>;
                        async fn get_block_number(&self) -> ports::l1::Result<L1Height>;
                        async fn balance(&self, address: ports::types::Address) -> ports::l1::Result<U256>;
                        async fn get_transaction_response(&self, tx_hash: [u8; 32]) -> ports::l1::Result<Option<TransactionResponse>>;
                        async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> ports::l1::Result<bool>;
                    }
                }
            }

            #[derive(Clone, Copy)]
            pub enum TxStatus {
                Success,
                Failure,
            }

            pub fn expects_state_submissions(
                expectations: impl IntoIterator<Item = (Option<NonEmpty<Fragment>>, [u8; 32])>,
            ) -> ports::l1::MockApi {
                let mut sequence = Sequence::new();

                let mut l1_mock = ports::l1::MockApi::new();

                for (fragment, tx_id) in expectations {
                    l1_mock
                        .expect_submit_state_fragments()
                        .withf(move |data, _previous_tx| {
                            if let Some(fragment) = &fragment {
                                data == fragment
                            } else {
                                true
                            }
                        })
                        .once()
                        .return_once(move |fragments, _previous_tx| {
                            Box::pin(async move {
                                Ok((
                                    ports::types::L1Tx {
                                        hash: tx_id,
                                        ..Default::default()
                                    },
                                    FragmentsSubmitted {
                                        num_fragments: min(fragments.len(), 6).try_into().unwrap(),
                                    },
                                ))
                            })
                        })
                        .in_sequence(&mut sequence);
                }

                l1_mock
            }

            pub fn txs_finished_multiple_heights(
                heights: &[u32],
                tx_height: u32,
                statuses: impl IntoIterator<Item = ([u8; 32], TxStatus)>,
            ) -> ports::l1::MockApi {
                let mut l1_mock = ports::l1::MockApi::new();

                for height in heights {
                    let l1_height = L1Height::from(*height);
                    l1_mock
                        .expect_get_block_number()
                        .times(1)
                        .returning(move || Box::pin(async move { Ok(l1_height) }));
                }

                for expectation in statuses {
                    let (tx_id, status) = expectation;

                    let height: u64 = tx_height.into();
                    l1_mock
                        .expect_get_transaction_response()
                        .with(eq(tx_id))
                        .returning(move |_| {
                            Box::pin(async move {
                                Ok(Some(TransactionResponse::new(
                                    height,
                                    matches!(status, TxStatus::Success),
                                )))
                            })
                        });
                }

                l1_mock
            }

            pub fn txs_finished(
                current_height: u32,
                tx_height: u32,
                statuses: impl IntoIterator<Item = ([u8; 32], TxStatus)>,
            ) -> ports::l1::MockApi {
                let mut l1_mock = ports::l1::MockApi::new();

                let height = L1Height::from(current_height);
                l1_mock
                    .expect_get_block_number()
                    .returning(move || Box::pin(async move { Ok(height) }));

                for expectation in statuses {
                    let (tx_id, status) = expectation;

                    let height = L1Height::from(tx_height);
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

            pub fn txs_reorg(
                heights: &[u32],
                tx_height: u32,
                first_status: ([u8; 32], TxStatus),
            ) -> ports::l1::MockApi {
                let mut l1_mock = ports::l1::MockApi::new();

                for height in heights {
                    let l1_height = L1Height::from(*height);
                    l1_mock
                        .expect_get_block_number()
                        .times(1)
                        .returning(move || Box::pin(async move { Ok(l1_height) }));
                }

                let (tx_id, status) = first_status;

                let height = L1Height::from(tx_height);
                l1_mock
                    .expect_get_transaction_response()
                    .with(eq(tx_id))
                    .times(1)
                    .return_once(move |_| {
                        Box::pin(async move {
                            Ok(Some(TransactionResponse::new(
                                height.into(),
                                matches!(status, TxStatus::Success),
                            )))
                        })
                    });

                l1_mock
                    .expect_get_transaction_response()
                    .with(eq(tx_id))
                    .times(1)
                    .return_once(move |_| Box::pin(async move { Ok(None) }));

                l1_mock
                    .expect_is_squeezed_out()
                    .with(eq(tx_id))
                    .times(1)
                    .return_once(move |_| Box::pin(async move { Ok(false) }));

                l1_mock
            }
        }

        pub mod fuel {
            use std::ops::RangeInclusive;

            use futures::{stream, StreamExt};
            use itertools::Itertools;
            use ports::{
                storage::SequentialFuelBlocks,
                types::{CollectNonEmpty, CompressedFuelBlock, NonEmpty},
            };
            use rand::{RngCore, SeedableRng};

            pub fn generate_block(height: u32, data_size: usize) -> CompressedFuelBlock {
                let mut small_rng = rand::rngs::SmallRng::from_seed([0; 32]);
                let mut buf = vec![0; data_size];
                small_rng.fill_bytes(&mut buf);

                let data = NonEmpty::collect(buf).expect("is not empty");

                CompressedFuelBlock { height, data }
            }

            pub fn generate_storage_block_sequence(
                heights: RangeInclusive<u32>,
                data_size: usize,
            ) -> SequentialFuelBlocks {
                heights
                    .map(|height| generate_block(height, data_size))
                    .collect_nonempty()
                    .unwrap()
                    .try_into()
                    .unwrap()
            }

            pub fn these_blocks_exist(
                blocks: impl IntoIterator<Item = CompressedFuelBlock>,
                enforce_tight_range: bool,
            ) -> ports::fuel::MockApi {
                let mut fuel_mock = ports::fuel::MockApi::default();

                let blocks = blocks
                    .into_iter()
                    .sorted_by_key(|b| b.height)
                    .collect::<Vec<_>>();

                let latest_block = blocks.last().expect("Must have at least one block").clone();

                let lowest_height = blocks.first().expect("Must have at least one block").height;
                let highest_height = latest_block.height;

                fuel_mock
                    .expect_latest_height()
                    .return_once(move || Box::pin(async move { Ok(highest_height) }));

                fuel_mock
                    .expect_compressed_blocks_in_height_range()
                    .returning(move |range| {
                        let expected_range = lowest_height..=highest_height;
                        if enforce_tight_range && range != expected_range {
                            panic!("range of requested blocks {range:?} is not as tight as expected: {expected_range:?}");
                        }

                        let blocks_vec: Vec<ports::fuel::Result<_>> = blocks
                            .iter()
                            .filter(move |b| range.contains(&b.height))
                            .cloned()
                            .map(Ok)
                            .collect();

                        stream::iter(blocks_vec).boxed()
                    });

                fuel_mock
            }

            pub fn latest_height_is(height: u32) -> ports::fuel::MockApi {
                let mut fuel_mock = ports::fuel::MockApi::default();
                fuel_mock
                    .expect_latest_height()
                    .returning(move || Box::pin(async move { Ok(height) }));
                fuel_mock
            }
        }
    }

    pub struct Setup {
        db: DbWithProcess,
    }

    impl Setup {
        pub async fn send_fragments(&self, eth_tx: [u8; 32]) {
            StateCommitter::new(
                mocks::l1::expects_state_submissions(vec![(None, eth_tx)]),
                mocks::fuel::latest_height_is(0),
                self.db(),
                crate::StateCommitterConfig::default(),
                TestClock::default(),
            )
            .run()
            .await
            .unwrap();
        }

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
            self.insert_fragments(0, 6).await;

            let clock = TestClock::default();
            clock.set_time(finalization_time);

            let tx = [1; 32];
            let l1_mock = mocks::l1::expects_state_submissions(vec![(None, tx)]);
            let fuel_mock = mocks::fuel::latest_height_is(0);
            let mut committer = StateCommitter::new(
                l1_mock,
                fuel_mock,
                self.db(),
                crate::StateCommitterConfig::default(),
                TestClock::default(),
            );
            committer.run().await.unwrap();

            let l1_mock = mocks::l1::txs_finished(0, 0, [(tx, TxStatus::Success)]);

            StateListener::new(
                l1_mock,
                self.db(),
                0,
                clock.clone(),
                IntGauge::new("test", "test").unwrap(),
            )
            .run()
            .await
            .unwrap();
        }

        pub async fn insert_fragments(&self, height: u32, amount: usize) -> Vec<Fragment> {
            let max_per_blob = (BlobEncoder::FRAGMENT_SIZE as f64 * 0.96) as usize;
            let fuel_blocks = self
                .import_blocks(Blocks::WithHeights {
                    range: height..=height,
                    data_size: amount.saturating_mul(max_per_blob),
                })
                .await;

            let factory = Factory::new(
                BlobEncoder,
                bundle::Encoder::new(CompressionLevel::Level6),
                1.try_into().unwrap(),
            );

            let mut fuel_api = ports::fuel::MockApi::new();
            let latest_height = fuel_blocks.last().height;
            fuel_api
                .expect_latest_height()
                .returning(move || Box::pin(async move { Ok(latest_height) }));

            let mut bundler = BlockBundler::new(
                fuel_api,
                self.db(),
                TestClock::default(),
                factory,
                BlockBundlerConfig {
                    optimization_time_limit: Duration::ZERO,
                    block_accumulation_time_limit: Duration::ZERO,
                    num_blocks_to_accumulate: 1.try_into().unwrap(),
                    lookback_window: 100,
                    ..Default::default()
                },
            );

            bundler.run().await.unwrap();

            let fragments = self
                .db
                .oldest_nonfinalized_fragments(0, amount)
                .await
                .unwrap();
            assert_eq!(fragments.len(), amount);

            fragments.into_iter().map(|f| f.fragment).collect()
        }

        pub async fn import_blocks(&self, blocks: Blocks) -> NonEmpty<CompressedFuelBlock> {
            let (mut block_importer, blocks) = self.block_importer(blocks);

            block_importer.run().await.unwrap();

            blocks
        }

        pub fn block_importer(
            &self,
            blocks: Blocks,
        ) -> (
            BlockImporter<DbWithProcess, ports::fuel::MockApi>,
            NonEmpty<CompressedFuelBlock>,
        ) {
            match blocks {
                Blocks::WithHeights { range, data_size } => {
                    let fuel_blocks = range
                        .map(|height| mocks::fuel::generate_block(height, data_size))
                        .collect_nonempty()
                        .unwrap();

                    let mock = mocks::fuel::these_blocks_exist(fuel_blocks.clone(), false);

                    (BlockImporter::new(self.db(), mock, 1000), fuel_blocks)
                }
            }
        }
    }

    pub enum Blocks {
        WithHeights {
            range: RangeInclusive<u32>,
            data_size: usize,
        },
    }
}
