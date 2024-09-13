use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use itertools::Itertools;
use ports::{
    clock::Clock,
    storage::{BundleFragment, Storage, ValidatedRange},
    types::{DateTime, Utc},
};
use tracing::{info, warn};

use crate::{Result, Runner};

pub struct StateCommitter<L1, Db, Clock> {
    l1_adapter: L1,
    storage: Db,
    clock: Clock,
    bundle_config: BundleGenerationConfig,
    component_created_at: DateTime<Utc>,
}

pub struct BundleGenerationConfig {
    pub acceptable_amount_of_blocks: ValidatedRange<usize>,
    pub accumulation_timeout: Duration,
}

impl<L1, Db, C: Clock> StateCommitter<L1, Db, C> {
    pub fn new(l1: L1, storage: Db, clock: C, bundle_config: BundleGenerationConfig) -> Self {
        let now = clock.now();
        Self {
            l1_adapter: l1,
            storage,
            clock,
            bundle_config,
            component_created_at: now,
        }
    }
}

impl<L1, Db, C> StateCommitter<L1, Db, C>
where
    L1: ports::l1::Api,
    Db: Storage,
    C: Clock,
{
    async fn submit_state(&self, fragment: BundleFragment) -> Result<()> {
        eprintln!("submitting state: {:?}", fragment);
        let tx = self.l1_adapter.submit_l2_state(fragment.data).await?;
        self.storage.record_pending_tx(tx, fragment.id).await?;

        Ok(())

        // // TODO: segfault, what about encoding overhead?
        // let (fragment_ids, data) = self.fetch_fragments().await?;
        //
        // // TODO: segfault what about when the fragments don't add up cleanly to max_total_size
        // if data.len() < max_total_size {
        //     let fragment_count = fragment_ids.len();
        //     let data_size = data.len();
        //     let remaining_space = max_total_size.saturating_sub(data_size);
        //
        //     let last_finalization = self
        //         .storage
        //         .last_time_a_fragment_was_finalized()
        //         .await?
        //         .unwrap_or_else(|| {
        //             info!("No fragment has been finalized yet, accumulation timeout will be calculated from the time the committer was started ({})", self.component_created_at);
        //             self.component_created_at
        //         });
        //
        //     let now = self.clock.now();
        //     let time_delta = now - last_finalization;
        //
        //     let duration = time_delta
        //         .to_std()
        //         .unwrap_or_else(|_| {
        //             warn!("possible time skew, last fragment finalization happened at {last_finalization}, with the current clock time at: {now} making for a difference of: {time_delta}");
        //             // we act as if the finalization happened now
        //             Duration::ZERO
        //         });
        //
        //     if duration < self.accumulation_timeout {
        //         info!("Found {fragment_count} fragment(s) with total size of {data_size}B. Waiting for additional fragments to use up more of the remaining {remaining_space}B.");
        //         return Ok(());
        //     } else {
        //         info!("Found {fragment_count} fragment(s) with total size of {data_size}B. Accumulation timeout has expired, proceeding to submit.")
        //     }
        // }
        //
        // if fragment_ids.is_empty() {
        //     return Ok(());
        // }
        //
        // let tx_hash = self.l1_adapter.submit_l2_state(data).await?;
        // self.storage
        //     .record_pending_tx(tx_hash, fragment_ids)
        //     .await?;
        //
        // info!("submitted blob tx {}", hex::encode(tx_hash));
        //
        // Ok(())
    }

    async fn is_tx_pending(&self) -> Result<bool> {
        self.storage.has_pending_txs().await.map_err(|e| e.into())
    }
}

#[async_trait]
impl<L1, Db, C> Runner for StateCommitter<L1, Db, C>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage,
    C: Send + Sync + Clock,
{
    async fn run(&mut self) -> Result<()> {
        println!("running state committer");
        if self.is_tx_pending().await? {
            println!("tx pending");
            return Ok(());
        };

        let fragment = if let Some(fragment) = self.storage.oldest_nonfinalized_fragment().await? {
            fragment
        } else {
            let max_blocks = self
                .bundle_config
                .acceptable_amount_of_blocks
                .inner()
                .clone()
                .max()
                .unwrap_or(0);
            let blocks: Vec<_> = self
                .storage
                .stream_unbundled_blocks()
                .take(max_blocks)
                .try_collect()
                .await?;

            if blocks.is_empty() {
                return Ok(());
            }

            if !self
                .bundle_config
                .acceptable_amount_of_blocks
                .contains(blocks.len())
            {
                return Ok(());
            }
            // TODO: segfault, change unwraps to ? wherever possible
            let merged_data = blocks
                .iter()
                .flat_map(|b| b.data.clone().into_inner())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap();
            let heights = blocks.iter().map(|b| b.height).collect::<Vec<_>>();

            let min_height = heights.iter().min().unwrap();
            let max_height = heights.iter().max().unwrap();

            let chunks = self
                .l1_adapter
                .split_into_submittable_state_chunks(&merged_data)?;

            let block_range = (*min_height..*max_height + 1).try_into().unwrap();

            self.storage
                .insert_bundle_and_fragments(block_range, chunks.clone())
                .await?
                .into_inner()
                .into_iter()
                .next()
                .expect("must have at least one element due to the usage of NonEmptyVec")
        };

        self.submit_state(fragment).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[allow(dead_code)]
    fn setup_logger() {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_level(true)
            .with_line_number(true)
            .json()
            .init();
    }

    use std::{
        num::NonZeroUsize,
        sync::{Arc, RwLock},
    };

    use clock::TestClock;
    use fuel_crypto::{Message, SecretKey, Signature};
    use mockall::{
        predicate::{self, eq},
        Sequence,
    };
    use ports::{
        fuel::{FuelBlock, FuelBlockId, FuelConsensus, FuelHeader, FuelPoAConsensus},
        l1::Api,
        types::{
            L1Height, NonEmptyVec, StateSubmission, TransactionResponse, TransactionState, U256,
        },
    };
    use storage::PostgresProcess;
    use tokio::sync::Mutex;
    use validator::BlockValidator;

    use crate::{StateImporter, StateListener};

    use super::*;

    // #[tokio::test]
    // async fn does_nothing_if_there_are_pending_transactions() -> Result<()> {
    //     //given
    //     let l1_mock = MockL1::new();
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //
    //     let block = FuelBlock {
    //         hash: [1; 32],
    //         height: 0,
    //         data: random_data(100.try_into().unwrap()),
    //     };
    //     db.insert_block(block.clone()).await?;
    //
    //     let range = (block.height..block.height + 1).try_into().unwrap();
    //
    //     db.insert_bundle_and_fragments(range, vec![block.data.clone()].try_into().unwrap())
    //         .await?;
    //
    //     let config = BundleGenerationConfig {
    //         acceptable_amount_of_blocks: (1..2).try_into().unwrap(),
    //         accumulation_timeout: Duration::from_secs(1),
    //     };
    //
    //     db.record_pending_tx([0; 32], 1.into()).await?;
    //
    //     let pending_txs = db.get_pending_txs().await?;
    //     dbg!(&pending_txs);
    //     let mut committer = StateCommitter::new(l1_mock, db.clone(), TestClock::default(), config);
    //
    //     // when
    //     committer.run().await.unwrap();
    //
    //     // then
    //     // mock didn't fail due to unexpected calls
    //     Ok(())
    // }
    //
    // // TODO: segfault add .once() to all tests since mocks dont fail by default if their
    // // expectations were not exercised, only if they were exercised incorrectly
    // #[tokio::test]
    // async fn fragments_available_block_and_sends_first_fragment() -> Result<()> {
    //     //given
    //     let block = ports::storage::FuelBlock {
    //         hash: [1; 32],
    //         height: 0,
    //         data: random_data(100.try_into().unwrap()),
    //     };
    //
    //     let l1_mock = MockL1::new();
    //
    //     let fragments: NonEmptyVec<NonEmptyVec<u8>> = vec![block.data.clone()].try_into().unwrap();
    //     {
    //         let fragments = fragments.clone();
    //         l1_mock
    //             .api
    //             .lock()
    //             .await
    //             .expect_split_into_submittable_state_chunks()
    //             .once()
    //             .with(eq(block.data.clone()))
    //             .return_once(move |_| Ok(fragments));
    //     }
    //
    //     l1_mock
    //         .api
    //         .lock()
    //         .await
    //         .expect_submit_l2_state()
    //         .once()
    //         .with(eq(fragments.inner()[0].clone()))
    //         .return_once(|_| Ok([1; 32]));
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_block(block.clone()).await?;
    //
    //     let config = BundleGenerationConfig {
    //         acceptable_amount_of_blocks: (1..2).try_into().unwrap(),
    //         accumulation_timeout: Duration::from_secs(1),
    //     };
    //
    //     let mut committer = StateCommitter::new(l1_mock, db.clone(), TestClock::default(), config);
    //
    //     // when
    //     committer.run().await.unwrap();
    //
    //     // then
    //     // mocks will validate the fragment was submitted
    //     let pending = db.get_pending_txs().await?;
    //     assert_eq!(pending.len(), 1);
    //     assert_eq!(pending[0].hash, [1; 32]);
    //
    //     Ok(())
    // }
    //
    fn random_data(size: usize) -> NonEmptyVec<u8> {
        if size == 0 {
            panic!("random data size must be greater than 0");
        }

        // TODO: segfault use better random data generation
        let data: Vec<u8> = (0..size).map(|_| rand::random::<u8>()).collect();

        data.try_into().expect("is not empty due to check")
    }

    pub mod mocks {
        pub mod l1 {
            use mockall::predicate::eq;
            use ports::types::{L1Height, TransactionResponse};

            pub fn tx_is_successful(tx_id: [u8; 32]) -> ports::l1::MockApi {
                let mut l1_mock = ports::l1::MockApi::new();

                let height = L1Height::from(0);
                l1_mock
                    .expect_get_block_number()
                    .returning(move || Ok(height));

                l1_mock
                    .expect_get_transaction_response()
                    .with(eq(tx_id))
                    .return_once(move |_| Ok(Some(TransactionResponse::new(height.into(), true))));
                l1_mock
            }
        }

        pub mod fuel {
            use fuel_crypto::{Message, SecretKey, Signature};
            use ports::fuel::{
                FuelBlock, FuelBlockId, FuelConsensus, FuelHeader, FuelPoAConsensus,
            };

            fn given_a_block(height: u32, secret_key: &SecretKey) -> ports::fuel::FuelBlock {
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

            pub fn block_exists(secret_key: SecretKey) -> ports::fuel::MockApi {
                let mut fuel_mock = ports::fuel::MockApi::default();
                let block = given_a_block(0, &secret_key);
                fuel_mock.expect_latest_block().return_once(|| Ok(block));
                fuel_mock
            }
        }
    }

    #[tokio::test]
    async fn sends_next_unsent_fragment() -> Result<()> {
        //given
        let db_process = PostgresProcess::shared().await.unwrap();
        let db = db_process.create_random_db().await?;

        let fragment_tx_ids = [[0; 32], [1; 32]];

        let mut tx_listener = {
            let l1_mock = mocks::l1::tx_is_successful(fragment_tx_ids[0]);

            StateListener::new(l1_mock, db.clone(), 0, TestClock::default())
        };

        let mut importer = {
            let secret_key = SecretKey::random(&mut rand::thread_rng());
            let block_validator = BlockValidator::new(*secret_key.public_key().hash());
            let fuel_mock = mocks::fuel::block_exists(secret_key);
            StateImporter::new(db.clone(), fuel_mock, block_validator, 1)
        };

        let mut sut = {
            let mut l1_mock = ports::l1::MockApi::new();

            let fragments = [
                random_data(100.try_into().unwrap()),
                random_data(100.try_into().unwrap()),
            ];

            {
                let fragments = fragments.clone();
                l1_mock
                    .expect_split_into_submittable_state_chunks()
                    .once()
                    .return_once(move |_| Ok(fragments.to_vec().try_into().unwrap()));
            }

            let mut sequence = Sequence::new();
            l1_mock
                .expect_submit_l2_state()
                .with(eq(fragments[0].clone()))
                .once()
                .return_once(move |_| Ok(fragment_tx_ids[0]))
                .in_sequence(&mut sequence);

            l1_mock
                .expect_submit_l2_state()
                .with(eq(fragments[1].clone()))
                .once()
                .return_once(move |_| Ok(fragment_tx_ids[1]))
                .in_sequence(&mut sequence);

            let bundle_config = BundleGenerationConfig {
                acceptable_amount_of_blocks: (1..2).try_into().unwrap(),
                accumulation_timeout: Duration::from_secs(1),
            };
            StateCommitter::new(l1_mock, db.clone(), TestClock::default(), bundle_config)
        };

        // imports the fuel block
        importer.run().await?;
        // sends the first fragment
        sut.run().await.unwrap();
        // reports the tx succeeded
        tx_listener.run().await.unwrap();

        // when
        sut.run().await.unwrap();

        // then
        // mocks validate that the second fragment has been sent after the first one

        Ok(())
    }

    // #[tokio::test]
    // async fn chooses_fragments_in_order() -> Result<()> {
    //     //given
    //     let block = ports::storage::FuelBlock {
    //         hash: [1; 32],
    //         height: 0,
    //         data: random_data(200.try_into().unwrap()),
    //     };
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_block(block.clone()).await?;
    //
    //     let range = (block.height..block.height + 1).try_into().unwrap();
    //
    //     let fragments: NonEmptyVec<NonEmptyVec<u8>> = vec![
    //         block.data.inner()[..100].to_vec().try_into().unwrap(),
    //         block.data.inner()[100..].to_vec().try_into().unwrap(),
    //     ]
    //     .try_into()
    //     .unwrap();
    //     db.insert_bundle_and_fragments(range, fragments.clone())
    //         .await?;
    //
    //     let mut l1_mock = MockL1::new();
    //
    //     l1_mock
    //         .api
    //         .expect_submit_l2_state()
    //         .once()
    //         .with(eq(fragments.inner()[0].clone()))
    //         .return_once(|_| Ok([1; 32]));
    //
    //     let config = BundleGenerationConfig {
    //         acceptable_amount_of_blocks: (1..2).try_into().unwrap(),
    //         accumulation_timeout: Duration::from_secs(1),
    //     };
    //
    //     let mut committer = StateCommitter::new(l1_mock, db.clone(), TestClock::default(), config);
    //
    //     // when
    //     committer.run().await.unwrap();
    //
    //     // then
    //     // mocks will validate the fragment was submitted
    //     let pending = db.get_pending_txs().await?;
    //     assert_eq!(pending.len(), 1);
    //     assert_eq!(pending[0].hash, [1; 32]);
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn chooses_fragments_from_older_bundle() -> Result<()> {
    //     //given
    //     let blocks = [
    //         ports::storage::FuelBlock {
    //             hash: [1; 32],
    //             height: 0,
    //             data: random_data(100.try_into().unwrap()),
    //         },
    //         ports::storage::FuelBlock {
    //             hash: [2; 32],
    //             height: 1,
    //             data: random_data(100.try_into().unwrap()),
    //         },
    //     ];
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_block(blocks[0].clone()).await?;
    //     db.insert_block(blocks[1].clone()).await?;
    //
    //     let range = (blocks[0].height..blocks[0].height + 1).try_into().unwrap();
    //
    //     let bundle_1_fragments: NonEmptyVec<NonEmptyVec<u8>> =
    //         vec![blocks[0].data.inner()[..100].to_vec().try_into().unwrap()]
    //             .try_into()
    //             .unwrap();
    //     db.insert_bundle_and_fragments(range, bundle_1_fragments.clone())
    //         .await?;
    //
    //     let range = (blocks[1].height..blocks[1].height + 1).try_into().unwrap();
    //     let bundle_2_fragments: NonEmptyVec<NonEmptyVec<u8>> =
    //         vec![blocks[1].data.inner()[..100].to_vec().try_into().unwrap()]
    //             .try_into()
    //             .unwrap();
    //     db.insert_bundle_and_fragments(range, bundle_2_fragments.clone())
    //         .await?;
    //
    //     let mut l1_mock = MockL1::new();
    //
    //     l1_mock
    //         .api
    //         .expect_submit_l2_state()
    //         .once()
    //         .with(eq(bundle_1_fragments.inner()[0].clone()))
    //         .return_once(|_| Ok([1; 32]));
    //
    //     let config = BundleGenerationConfig {
    //         acceptable_amount_of_blocks: (1..2).try_into().unwrap(),
    //         accumulation_timeout: Duration::from_secs(1),
    //     };
    //
    //     let mut committer = StateCommitter::new(l1_mock, db.clone(), TestClock::default(), config);
    //
    //     // when
    //     committer.run().await.unwrap();
    //
    //     // then
    //     // mocks will validate the fragment was submitted
    //     let pending = db.get_pending_txs().await?;
    //     assert_eq!(pending.len(), 1);
    //     assert_eq!(pending[0].hash, [1; 32]);
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn repeats_failed_fragments() -> Result<()> {
    //     //given
    //     let block = ports::storage::FuelBlock {
    //         hash: [1; 32],
    //         height: 0,
    //         data: random_data(200.try_into().unwrap()),
    //     };
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_block(block.clone()).await?;
    //
    //     let range = (block.height..block.height + 1).try_into().unwrap();
    //
    //     let fragments: NonEmptyVec<NonEmptyVec<u8>> = vec![
    //         block.data.inner()[..100].to_vec().try_into().unwrap(),
    //         block.data.inner()[100..].to_vec().try_into().unwrap(),
    //     ]
    //     .try_into()
    //     .unwrap();
    //     let fragments = db
    //         .insert_bundle_and_fragments(range, fragments.clone())
    //         .await?;
    //
    //     let mut l1_mock = MockL1::new();
    //     db.record_pending_tx([0; 32], fragments.inner()[0].id)
    //         .await?;
    //     db.update_tx_state([0; 32], TransactionState::Failed)
    //         .await?;
    //
    //     l1_mock
    //         .api
    //         .expect_submit_l2_state()
    //         .once()
    //         .with(eq(fragments.inner()[0].data.clone()))
    //         .return_once(|_| Ok([1; 32]));
    //
    //     let config = BundleGenerationConfig {
    //         acceptable_amount_of_blocks: (1..2).try_into().unwrap(),
    //         accumulation_timeout: Duration::from_secs(1),
    //     };
    //
    //     let mut committer = StateCommitter::new(l1_mock, db.clone(), TestClock::default(), config);
    //
    //     // when
    //     committer.run().await.unwrap();
    //
    //     // then
    //     // mocks will validate the fragment was submitted
    //     let pending = db.get_pending_txs().await?;
    //     assert_eq!(pending.len(), 1);
    //     assert_eq!(pending[0].hash, [1; 32]);
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn does_nothing_if_not_enough_blocks() -> Result<()> {
    //     //given
    //     let block = ports::storage::FuelBlock {
    //         hash: [1; 32],
    //         height: 0,
    //         data: random_data(200.try_into().unwrap()),
    //     };
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_block(block.clone()).await?;
    //
    //     let mut l1_mock = MockL1::new();
    //
    //     let config = BundleGenerationConfig {
    //         acceptable_amount_of_blocks: (2..3).try_into().unwrap(),
    //         accumulation_timeout: Duration::from_secs(1),
    //     };
    //
    //     let mut committer = StateCommitter::new(l1_mock, db.clone(), TestClock::default(), config);
    //
    //     // when
    //     committer.run().await.unwrap();
    //
    //     // then
    //     // mocks will validate nothing happened
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn bundles_minimum_if_no_more_blocks_available() -> Result<()> {
    //     //given
    //     let blocks = [
    //         ports::storage::FuelBlock {
    //             hash: [1; 32],
    //             height: 0,
    //             data: random_data(200.try_into().unwrap()),
    //         },
    //         ports::storage::FuelBlock {
    //             hash: [2; 32],
    //             height: 1,
    //             data: random_data(200.try_into().unwrap()),
    //         },
    //     ];
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_block(blocks[0].clone()).await?;
    //     db.insert_block(blocks[1].clone()).await?;
    //
    //     let mut l1_mock = MockL1::new();
    //     let merged_data: NonEmptyVec<u8> = [
    //         blocks[0].data.clone().into_inner(),
    //         blocks[1].data.clone().into_inner(),
    //     ]
    //     .concat()
    //     .try_into()
    //     .unwrap();
    //     l1_mock
    //         .api
    //         .expect_split_into_submittable_state_chunks()
    //         .once()
    //         .with(eq(merged_data.clone()))
    //         .return_once(|data| Ok(vec![data.clone()].try_into().unwrap()));
    //
    //     let config = BundleGenerationConfig {
    //         acceptable_amount_of_blocks: (2..3).try_into().unwrap(),
    //         accumulation_timeout: Duration::from_secs(1),
    //     };
    //
    //     l1_mock
    //         .api
    //         .expect_submit_l2_state()
    //         .with(eq(merged_data))
    //         .once()
    //         .return_once(|_| Ok([1; 32]));
    //
    //     let mut committer = StateCommitter::new(l1_mock, db.clone(), TestClock::default(), config);
    //
    //     // when
    //     committer.run().await.unwrap();
    //
    //     // then
    //     assert!(db.has_pending_txs().await?);
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn doesnt_bundle_more_than_maximum_blocks() -> Result<()> {
    //     //given
    //     let blocks = [
    //         ports::storage::FuelBlock {
    //             hash: [1; 32],
    //             height: 0,
    //             data: random_data(200.try_into().unwrap()),
    //         },
    //         ports::storage::FuelBlock {
    //             hash: [2; 32],
    //             height: 1,
    //             data: random_data(200.try_into().unwrap()),
    //         },
    //     ];
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_block(blocks[0].clone()).await?;
    //     db.insert_block(blocks[1].clone()).await?;
    //
    //     let mut l1_mock = MockL1::new();
    //     let data = blocks[0].data.clone();
    //     l1_mock
    //         .api
    //         .expect_split_into_submittable_state_chunks()
    //         .once()
    //         .with(eq(data.clone()))
    //         .return_once(|data| Ok(vec![data.clone()].try_into().unwrap()));
    //
    //     let config = BundleGenerationConfig {
    //         acceptable_amount_of_blocks: (1..2).try_into().unwrap(),
    //         accumulation_timeout: Duration::from_secs(1),
    //     };
    //
    //     l1_mock
    //         .api
    //         .expect_submit_l2_state()
    //         .with(eq(data))
    //         .once()
    //         .return_once(|_| Ok([1; 32]));
    //
    //     let mut committer = StateCommitter::new(l1_mock, db.clone(), TestClock::default(), config);
    //
    //     // when
    //     committer.run().await.unwrap();
    //
    //     // then
    //     assert!(db.has_pending_txs().await?);
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn doesnt_bundle_already_bundled_blocks() -> Result<()> {
    //     //given
    //     let blocks = [
    //         ports::storage::FuelBlock {
    //             hash: [1; 32],
    //             height: 0,
    //             data: random_data(200.try_into().unwrap()),
    //         },
    //         ports::storage::FuelBlock {
    //             hash: [2; 32],
    //             height: 1,
    //             data: random_data(200.try_into().unwrap()),
    //         },
    //     ];
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_block(blocks[0].clone()).await?;
    //     db.insert_block(blocks[1].clone()).await?;
    //
    //     let mut l1_mock = MockL1::new();
    //     let data = blocks[1].data.clone();
    //     l1_mock
    //         .api
    //         .expect_split_into_submittable_state_chunks()
    //         .once()
    //         .with(eq(data.clone()))
    //         .return_once(|data| Ok(vec![data.clone()].try_into().unwrap()));
    //
    //     let config = BundleGenerationConfig {
    //         acceptable_amount_of_blocks: (1..2).try_into().unwrap(),
    //         accumulation_timeout: Duration::from_secs(1),
    //     };
    //
    //     let fragments = db
    //         .insert_bundle_and_fragments(
    //             (0..1).try_into().unwrap(),
    //             vec![data.clone()].try_into().unwrap(),
    //         )
    //         .await?;
    //     db.record_pending_tx([0; 32], fragments.inner()[0].id)
    //         .await?;
    //     db.update_tx_state([0; 32], TransactionState::Finalized(Utc::now()))
    //         .await?;
    //
    //     l1_mock
    //         .api
    //         .expect_submit_l2_state()
    //         .with(eq(data))
    //         .once()
    //         .return_once(|_| Ok([1; 32]));
    //
    //     let mut committer = StateCommitter::new(l1_mock, db.clone(), TestClock::default(), config);
    //
    //     // when
    //     committer.run().await.unwrap();
    //
    //     // then
    //     assert!(db.has_pending_txs().await?);
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn handles_empty_range() -> Result<()> {
    //     //given
    //     let blocks = [ports::storage::FuelBlock {
    //         hash: [1; 32],
    //         height: 0,
    //         data: random_data(200.try_into().unwrap()),
    //     }];
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_block(blocks[0].clone()).await?;
    //
    //     let l1_mock = MockL1::new();
    //
    //     let config = BundleGenerationConfig {
    //         acceptable_amount_of_blocks: (0..1).try_into().unwrap(),
    //         accumulation_timeout: Duration::from_secs(1),
    //     };
    //
    //     let mut committer = StateCommitter::new(l1_mock, db.clone(), TestClock::default(), config);
    //
    //     // when
    //     committer.run().await.unwrap();
    //
    //     // then
    //     // no calls to mocks were made
    //     assert!(!db.has_pending_txs().await?);
    //
    //     Ok(())
    // }
    //
    // // #[tokio::test]
    // // async fn will_wait_for_more_data() -> Result<()> {
    // //     // given
    // //     let (block_1_state, block_1_state_fragment) = (
    // //         StateSubmission {
    // //             id: None,
    // //             block_hash: [0u8; 32],
    // //             block_height: 1,
    // //         },
    // //         StateFragment {
    // //             id: None,
    // //             submission_id: None,
    // //             fragment_idx: 0,
    // //             data: vec![0; 127_000],
    // //             created_at: ports::types::Utc::now(),
    // //         },
    // //     );
    // //     let l1_mock = MockL1::new();
    // //
    // //     let process = PostgresProcess::shared().await.unwrap();
    // //     let db = process.create_random_db().await?;
    // //     db.insert_state_submission(block_1_state, vec![block_1_state_fragment])
    // //         .await?;
    // //
    // //     let mut committer = StateCommitter::new(
    // //         l1_mock,
    // //         db.clone(),
    // //         TestClock::default(),
    // //         Duration::from_secs(1),
    // //     );
    // //
    // //     // when
    // //     committer.run().await.unwrap();
    // //
    // //     // then
    // //     assert!(!db.has_pending_txs().await?);
    // //
    // //     Ok(())
    // // }
    // //
    // // #[tokio::test]
    // // async fn triggers_when_enough_data_is_made_available() -> Result<()> {
    // //     // given
    // //     let max_data = 6 * 128 * 1024;
    // //     let (block_1_state, block_1_state_fragment) = (
    // //         StateSubmission {
    // //             id: None,
    // //             block_hash: [0u8; 32],
    // //             block_height: 1,
    // //         },
    // //         StateFragment {
    // //             id: None,
    // //             submission_id: None,
    // //             fragment_idx: 0,
    // //             data: vec![1; max_data - 1000],
    // //             created_at: ports::types::Utc::now(),
    // //         },
    // //     );
    // //
    // //     let (block_2_state, block_2_state_fragment) = (
    // //         StateSubmission {
    // //             id: None,
    // //             block_hash: [1u8; 32],
    // //             block_height: 2,
    // //         },
    // //         StateFragment {
    // //             id: None,
    // //             submission_id: None,
    // //             fragment_idx: 0,
    // //             data: vec![1; 1000],
    // //             created_at: ports::types::Utc::now(),
    // //         },
    // //     );
    // //     let l1_mock = given_l1_that_expects_submission(
    // //         [
    // //             block_1_state_fragment.data.clone(),
    // //             block_2_state_fragment.data.clone(),
    // //         ]
    // //         .concat(),
    // //     );
    // //
    // //     let process = PostgresProcess::shared().await.unwrap();
    // //     let db = process.create_random_db().await?;
    // //     db.insert_state_submission(block_1_state, vec![block_1_state_fragment])
    // //         .await?;
    // //
    // //     let mut committer = StateCommitter::new(
    // //         l1_mock,
    // //         db.clone(),
    // //         TestClock::default(),
    // //         Duration::from_secs(1),
    // //     );
    // //     committer.run().await?;
    // //     assert!(!db.has_pending_txs().await?);
    // //     assert!(db.get_pending_txs().await?.is_empty());
    // //
    // //     db.insert_state_submission(block_2_state, vec![block_2_state_fragment])
    // //         .await?;
    // //     tokio::time::sleep(Duration::from_millis(2000)).await;
    // //
    // //     // when
    // //     committer.run().await?;
    // //
    // //     // then
    // //     assert!(!db.get_pending_txs().await?.is_empty());
    // //     assert!(db.has_pending_txs().await?);
    // //
    // //     Ok(())
    // // }
    // //
    // // #[tokio::test]
    // // async fn will_trigger_on_accumulation_timeout() -> Result<()> {
    // //     // given
    // //     let (block_1_state, block_1_submitted_fragment, block_1_unsubmitted_state_fragment) = (
    // //         StateSubmission {
    // //             id: None,
    // //             block_hash: [0u8; 32],
    // //             block_height: 1,
    // //         },
    // //         StateFragment {
    // //             id: None,
    // //             submission_id: None,
    // //             fragment_idx: 0,
    // //             data: vec![0; 100],
    // //             created_at: ports::types::Utc::now(),
    // //         },
    // //         StateFragment {
    // //             id: None,
    // //             submission_id: None,
    // //             fragment_idx: 0,
    // //             data: vec![0; 127_000],
    // //             created_at: ports::types::Utc::now(),
    // //         },
    // //     );
    // //
    // //     let l1_mock =
    // //         given_l1_that_expects_submission(block_1_unsubmitted_state_fragment.data.clone());
    // //
    // //     let process = PostgresProcess::shared().await.unwrap();
    // //     let db = process.create_random_db().await?;
    // //     db.insert_state_submission(
    // //         block_1_state,
    // //         vec![
    // //             block_1_submitted_fragment,
    // //             block_1_unsubmitted_state_fragment,
    // //         ],
    // //     )
    // //     .await?;
    // //
    // //     let clock = TestClock::default();
    // //
    // //     db.record_pending_tx([0; 32], vec![1]).await?;
    // //     db.update_submission_tx_state([0; 32], TransactionState::Finalized(clock.now()))
    // //         .await?;
    // //
    // //     let accumulation_timeout = Duration::from_secs(1);
    // //     let mut committer =
    // //         StateCommitter::new(l1_mock, db.clone(), clock.clone(), accumulation_timeout);
    // //     committer.run().await?;
    // //     // No pending tx since we have not accumulated enough data nor did the timeout expire
    // //     assert!(!db.has_pending_txs().await?);
    // //
    // //     clock.adv_time(Duration::from_secs(1)).await;
    // //
    // //     // when
    // //     committer.run().await?;
    // //
    // //     // then
    // //     assert!(db.has_pending_txs().await?);
    // //
    // //     Ok(())
    // // }
}
