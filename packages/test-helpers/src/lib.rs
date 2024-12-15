#![deny(unused_crate_dependencies)]

use std::{ops::RangeInclusive, time::Duration};

use clock::TestClock;
use eth::BlobEncoder;
use fuel_block_committer_encoding::bundle::{self, CompressionLevel};
use metrics::prometheus::IntGauge;
use mocks::l1::TxStatus;
use rand::{Rng, RngCore};
use services::fee_analytics::port::l1::testing::TestFeesProvider;
use services::fee_analytics::service::FeeAnalytics;
use services::types::{
    BlockSubmission, CollectNonEmpty, CompressedFuelBlock, Fragment, L1Tx, NonEmpty,
};
use storage::{DbWithProcess, PostgresProcess};

use services::{block_committer::service::BlockCommitter, Runner};
use services::{
    block_importer::service::BlockImporter, state_listener::service::StateListener, BlockBundler,
    BlockBundlerConfig, BundlerFactory, StateCommitter,
};

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

        use std::{cmp::min, num::NonZeroU32};

        use delegate::delegate;
        use mockall::{predicate::eq, Sequence};
        use services::types::{
            BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Height, L1Tx, NonEmpty,
            TransactionResponse,
        };

        pub struct FullL1Mock {
            pub state_listener_l1_api: services::state_listener::port::l1::MockApi,
            pub block_committer_l1_api: services::block_committer::port::l1::MockApi,
            pub block_committer_contract: services::block_committer::port::l1::MockContract,
        }

        impl Default for FullL1Mock {
            fn default() -> Self {
                Self::new()
            }
        }

        impl FullL1Mock {
            pub fn new() -> Self {
                Self {
                    state_listener_l1_api: services::state_listener::port::l1::MockApi::new(),
                    block_committer_l1_api: services::block_committer::port::l1::MockApi::new(),
                    block_committer_contract:
                        services::block_committer::port::l1::MockContract::new(),
                }
            }
        }

        impl services::block_committer::port::l1::Contract for FullL1Mock {
            delegate! {
                to self.block_committer_contract {
                    async fn submit(&self, hash: [u8;32], height: u32) -> services::Result<BlockSubmissionTx>;
                    fn commit_interval(&self) -> NonZeroU32;
                }
            }
        }

        impl services::block_committer::port::l1::Api for FullL1Mock {
            delegate! {
                to self.block_committer_l1_api {
                    async fn get_block_number(&self) -> services::Result<L1Height>;
                    async fn get_transaction_response(
                        &self,
                        tx_hash: [u8; 32],
                    ) -> services::Result<Option<TransactionResponse>>;
                }
            }
        }

        impl services::state_listener::port::l1::Api for FullL1Mock {
            delegate! {
                to self.state_listener_l1_api {
                    async fn get_block_number(&self) -> services::Result<L1Height>;
                    async fn get_transaction_response(
                        &self,
                        tx_hash: [u8; 32],
                    ) -> services::Result<Option<TransactionResponse>>;
                    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> services::Result<bool>;
                }
            }
        }

        #[derive(Clone, Copy)]
        pub enum TxStatus {
            Success,
            Failure,
        }

        pub fn expects_contract_submission(
            block: services::types::fuel::FuelBlock,
            eth_tx: [u8; 32],
        ) -> FullL1Mock {
            let mut l1 = FullL1Mock::new();

            let submission_tx = BlockSubmissionTx {
                hash: eth_tx,
                ..Default::default()
            };
            l1.block_committer_contract
                .expect_submit()
                .withf(move |hash, height| *hash == *block.id && *height == block.header.height)
                .return_once(move |_, _| Box::pin(async { Ok(submission_tx) }))
                .once();

            l1.block_committer_l1_api
                .expect_get_block_number()
                .return_once(move || Box::pin(async { Ok(0u32.into()) }));

            l1
        }

        pub fn expects_transaction_response(
            block_number: u32,
            tx_hash: [u8; 32],
            response: Option<TransactionResponse>,
        ) -> FullL1Mock {
            let mut l1 = FullL1Mock::new();

            l1.block_committer_l1_api
                .expect_get_block_number()
                .returning(move || Box::pin(async move { Ok(block_number.into()) }));

            l1.block_committer_l1_api
                .expect_get_transaction_response()
                .with(eq(tx_hash))
                .returning(move |_| {
                    let response = response.clone();
                    Box::pin(async move { Ok(response) })
                });

            l1.block_committer_contract.expect_submit().never();

            l1
        }

        pub fn expects_state_submissions(
            expectations: impl IntoIterator<Item = (Option<NonEmpty<Fragment>>, L1Tx)>,
        ) -> services::state_committer::port::l1::MockApi {
            let mut sequence = Sequence::new();

            let mut l1_mock = services::state_committer::port::l1::MockApi::new();

            for (fragment, tx) in expectations {
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
                                tx,
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
        ) -> services::state_listener::port::l1::MockApi {
            let mut l1_mock = services::state_listener::port::l1::MockApi::new();

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
                                100,
                                100,
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
        ) -> services::state_listener::port::l1::MockApi {
            let mut l1_mock = services::state_listener::port::l1::MockApi::new();

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
                                100,
                                100,
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
        ) -> services::state_listener::port::l1::MockApi {
            let mut l1_mock = services::state_listener::port::l1::MockApi::new();

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
                            100,
                            100,
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

        use fuel_crypto::{Message, SecretKey, Signature};
        use futures::{stream, StreamExt};
        use itertools::Itertools;
        use mockall::predicate::eq;
        use rand::{rngs::StdRng, RngCore, SeedableRng};
        use services::types::{
            storage::SequentialFuelBlocks, CollectNonEmpty, CompressedFuelBlock, NonEmpty,
        };

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
        ) -> services::block_importer::port::fuel::MockApi {
            let mut fuel_mock = services::block_importer::port::fuel::MockApi::default();

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

                        let blocks_vec: Vec<services::Result<_>> = blocks
                            .iter()
                            .filter(move |b| range.contains(&b.height))
                            .cloned()
                            .map(Ok)
                            .collect();

                        stream::iter(blocks_vec).boxed()
                    });

            fuel_mock
        }

        pub fn latest_height_is(height: u32) -> services::state_committer::port::fuel::MockApi {
            let mut fuel_mock = services::state_committer::port::fuel::MockApi::default();
            fuel_mock
                .expect_latest_height()
                .returning(move || Box::pin(async move { Ok(height) }));

            fuel_mock
        }

        pub fn block_bundler_latest_height_is(
            height: u32,
        ) -> services::block_bundler::port::fuel::MockApi {
            let mut fuel_mock = services::block_bundler::port::fuel::MockApi::default();
            fuel_mock
                .expect_latest_height()
                .returning(move || Box::pin(async move { Ok(height) }));

            fuel_mock
        }

        pub fn given_fetcher(
            available_blocks: Vec<services::types::fuel::FuelBlock>,
        ) -> services::block_committer::port::fuel::MockApi {
            let mut fetcher = services::block_committer::port::fuel::MockApi::new();
            for block in available_blocks.clone() {
                fetcher
                    .expect_block_at_height()
                    .with(eq(block.header.height))
                    .returning(move |_| {
                        let block = block.clone();
                        Box::pin(async move { Ok(Some(block)) })
                    });
            }
            if let Some(block) = available_blocks
                .into_iter()
                .max_by_key(|el| el.header.height)
            {
                fetcher.expect_latest_block().returning(move || {
                    let block = block.clone();
                    Box::pin(async { Ok(block) })
                });
            }

            fetcher
        }

        fn given_header(height: u32) -> services::types::fuel::FuelHeader {
            let application_hash =
                "0x017ab4b70ea129c29e932d44baddc185ad136bf719c4ada63a10b5bf796af91e"
                    .parse()
                    .unwrap();

            services::types::fuel::FuelHeader {
                id: Default::default(),
                da_height: Default::default(),
                consensus_parameters_version: Default::default(),
                state_transition_bytecode_version: Default::default(),
                transactions_count: Default::default(),
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

        pub fn given_secret_key() -> SecretKey {
            let mut rng = StdRng::seed_from_u64(42);

            SecretKey::random(&mut rng)
        }

        pub fn given_a_block(
            height: u32,
            secret_key: &SecretKey,
        ) -> services::types::fuel::FuelBlock {
            let header = given_header(height);

            let mut hasher = fuel_crypto::Hasher::default();
            hasher.input(header.prev_root.as_ref());
            hasher.input(header.height.to_be_bytes());
            hasher.input(header.time.0.to_be_bytes());
            hasher.input(header.application_hash.as_ref());

            let id = services::types::fuel::FuelBlockId::from(hasher.digest());
            let id_message = Message::from_bytes(*id);
            let signature = Signature::sign(secret_key, &id_message);

            services::types::fuel::FuelBlock {
                id,
                header,
                consensus: services::types::fuel::FuelConsensus::PoAConsensus(
                    services::types::fuel::FuelPoAConsensus { signature },
                ),
                transactions: vec![],
                block_producer: Some(secret_key.public_key()),
            }
        }
    }
}

pub struct Setup {
    db: DbWithProcess,
    test_clock: TestClock,
}

impl Setup {
    pub async fn init() -> Self {
        let db = PostgresProcess::shared()
            .await
            .unwrap()
            .create_random_db()
            .await
            .unwrap();

        Self {
            db,
            test_clock: TestClock::default(),
        }
    }

    pub fn db(&self) -> DbWithProcess {
        self.db.clone()
    }

    pub fn test_clock(&self) -> TestClock {
        self.test_clock.clone()
    }

    pub async fn submit_contract_transaction(&self, height: u32, eth_tx: [u8; 32]) {
        let secret_key = mocks::fuel::given_secret_key();
        let latest_block = mocks::fuel::given_a_block(height, &secret_key);
        let fuel_adapter = mocks::fuel::given_fetcher(vec![latest_block.clone()]);

        let l1 = mocks::l1::expects_contract_submission(latest_block, eth_tx);
        let mut block_committer = BlockCommitter::new(
            l1,
            self.db(),
            fuel_adapter,
            self.test_clock(),
            10.try_into().unwrap(),
            1,
        );

        block_committer.run().await.unwrap();
    }

    pub async fn send_fragments(&self, eth_tx: [u8; 32], eth_nonce: u32) {
        StateCommitter::new(
            mocks::l1::expects_state_submissions(vec![(
                None,
                L1Tx {
                    hash: eth_tx,
                    nonce: eth_nonce,
                    ..Default::default()
                },
            )]),
            mocks::fuel::latest_height_is(0),
            self.db(),
            services::StateCommitterConfig {
                lookback_window: 1000,
                fragment_accumulation_timeout: Duration::from_secs(0),
                fragments_to_accumulate: 1.try_into().unwrap(),
                gas_bump_timeout: Duration::from_secs(300),
                tx_max_fee: 1_000_000_000,
            },
            self.test_clock.clone(),
            FeeAnalytics::new(TestFeesProvider::new(vec![])),
        )
        .run()
        .await
        .unwrap();
    }

    pub async fn commit_single_block_bundle(&self) {
        self.commit_block_bundle([1; 32], 0, 0).await
    }

    pub async fn commit_block_bundle(&self, eth_tx: [u8; 32], eth_nonce: u32, height: u32) {
        self.insert_fragments(height, 6).await;

        let l1_mock = mocks::l1::expects_state_submissions(vec![(
            None,
            L1Tx {
                hash: eth_tx,
                nonce: eth_nonce,
                ..Default::default()
            },
        )]);
        let fuel_mock = mocks::fuel::latest_height_is(height);
        let mut committer = StateCommitter::new(
            l1_mock,
            fuel_mock,
            self.db(),
            services::StateCommitterConfig {
                lookback_window: 1000,
                fragment_accumulation_timeout: Duration::from_secs(0),
                fragments_to_accumulate: 1.try_into().unwrap(),
                gas_bump_timeout: Duration::from_secs(300),
                tx_max_fee: 1_000_000_000,
            },
            self.test_clock.clone(),
            FeeAnalytics::new(TestFeesProvider::new(vec![])),
        );
        committer.run().await.unwrap();

        let l1_mock = mocks::l1::txs_finished(0, 0, [(eth_tx, TxStatus::Success)]);

        StateListener::new(
            l1_mock,
            self.db(),
            0,
            self.test_clock.clone(),
            IntGauge::new("test", "test").unwrap(),
        )
        .run()
        .await
        .unwrap();
    }

    pub async fn insert_fragments(&self, height: u32, amount: usize) -> Vec<Fragment> {
        use services::state_committer::port::Storage;

        let max_per_blob = (BlobEncoder::FRAGMENT_SIZE as f64 * 0.96) as usize;
        let fuel_blocks = self
            .import_blocks(Blocks::WithHeights {
                range: height..=height,
                data_size: amount.saturating_mul(max_per_blob),
            })
            .await;

        let factory = BundlerFactory::new(
            BlobEncoder,
            bundle::Encoder::new(CompressionLevel::Level6),
            1.try_into().unwrap(),
        );

        let mut fuel_api = services::block_bundler::port::fuel::MockApi::new();
        let latest_height = fuel_blocks.last().height;
        assert_eq!(height, latest_height);
        fuel_api
            .expect_latest_height()
            .returning(move || Box::pin(async move { Ok(latest_height) }));

        let mut bundler = BlockBundler::new(
            fuel_api,
            self.db(),
            self.test_clock.clone(),
            factory,
            BlockBundlerConfig {
                optimization_time_limit: Duration::ZERO,
                block_accumulation_time_limit: Duration::ZERO,
                num_blocks_to_accumulate: 1.try_into().unwrap(),
                lookback_window: 100,
                max_bundles_per_optimization_run: 1.try_into().unwrap(),
            },
        );

        bundler.run().await.unwrap();

        let fragments = self
            .db
            .oldest_nonfinalized_fragments(height, amount)
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
        BlockImporter<DbWithProcess, services::block_importer::port::fuel::MockApi>,
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

    pub fn given_incomplete_submission(block_height: u32) -> BlockSubmission {
        let mut submission: BlockSubmission = rand::thread_rng().gen();
        submission.block_height = block_height;
        submission.completed = false;

        submission
    }

    // TODO: we should use the block committer here to insert the submissions
    pub async fn add_submissions(&self, pending_submissions: Vec<u32>) {
        use services::block_committer::port::Storage;

        for height in pending_submissions {
            let submission_tx = services::types::BlockSubmissionTx {
                hash: [height as u8; 32],
                nonce: height,
                ..Default::default()
            };
            self.db
                .record_block_submission(
                    submission_tx,
                    Self::given_incomplete_submission(height),
                    self.test_clock.now(),
                )
                .await
                .unwrap();
        }
    }
}

pub enum Blocks {
    WithHeights {
        range: RangeInclusive<u32>,
        data_size: usize,
    },
}
