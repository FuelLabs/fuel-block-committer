use std::num::NonZeroU32;

use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use ports::{
    fuel::FuelBlock,
    storage::Storage,
    types::{BlockSubmission, NonNegative, TransactionState},
};
use tracing::info;

use super::Runner;
use crate::{validator::Validator, Error, Result};

pub struct BlockCommitter<L1, Db, Fuel, BlockValidator, Clock> {
    l1_adapter: L1,
    fuel_adapter: Fuel,
    storage: Db,
    block_validator: BlockValidator,
    clock: Clock,
    commit_interval: NonZeroU32,
    num_blocks_to_finalize_tx: u64,
    metrics: Metrics,
}

#[derive(Debug)]
enum Action {
    UpdateTx {
        submission_id: NonNegative<i32>,
        block_height: u32,
    },
    Post,
    DoNothing,
}

struct Metrics {
    latest_fuel_block: IntGauge,
    latest_committed_block: IntGauge,
}

impl<L1, Db, Fuel, BlockValidator, Clock> RegistersMetrics
    for BlockCommitter<L1, Db, Fuel, BlockValidator, Clock>
{
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.metrics.latest_fuel_block.clone()),
            Box::new(self.metrics.latest_committed_block.clone()),
        ]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let latest_fuel_block = IntGauge::with_opts(Opts::new(
            "latest_fuel_block",
            "The height of the latest fuel block.",
        ))
        .expect("fuel_network_errors metric to be correctly configured");

        let latest_committed_block = IntGauge::with_opts(Opts::new(
            "latest_committed_block",
            "The height of the latest fuel block committed on Ethereum.",
        ))
        .expect("latest_committed_block metric to be correctly configured");

        Self {
            latest_fuel_block,
            latest_committed_block,
        }
    }
}

impl<L1, Db, Fuel, BlockValidator, Clock> BlockCommitter<L1, Db, Fuel, BlockValidator, Clock> {
    pub fn new(
        l1: L1,
        storage: Db,
        fuel_adapter: Fuel,
        block_validator: BlockValidator,
        clock: Clock,
        commit_interval: NonZeroU32,
        num_blocks_to_finalize_tx: u64,
    ) -> Self {
        Self {
            l1_adapter: l1,
            storage,
            fuel_adapter,
            block_validator,
            clock,
            commit_interval,
            num_blocks_to_finalize_tx,
            metrics: Metrics::default(),
        }
    }
}

impl<L1, Db, Fuel, BlockValidator, Clock> BlockCommitter<L1, Db, Fuel, BlockValidator, Clock>
where
    L1: ports::l1::Contract + ports::l1::Api,
    Db: Storage,
    BlockValidator: Validator,
    Fuel: ports::fuel::Api,
    Clock: ports::clock::Clock,
{
    async fn submit_block(&self, fuel_block: FuelBlock) -> Result<()> {
        let submission = BlockSubmission::new(*fuel_block.id, fuel_block.header.height);

        let mut tx = self
            .l1_adapter
            .submit(*fuel_block.id, fuel_block.header.height)
            .await?;
        tx.submission_id = submission.id;
        self.storage.record_block_submission(tx, submission).await?;

        info!("submitted {fuel_block:?}!");

        Ok(())
    }

    async fn fetch_latest_block(&self) -> Result<FuelBlock> {
        let latest_block = self.fuel_adapter.latest_block().await?;
        self.block_validator.validate(
            latest_block.id,
            &latest_block.header,
            &latest_block.consensus,
        )?;

        self.metrics
            .latest_fuel_block
            .set(i64::from(latest_block.header.height));

        Ok(latest_block)
    }

    fn current_epoch_block_height(&self, current_block_height: u32) -> u32 {
        current_block_height - (current_block_height % self.commit_interval)
    }

    async fn fetch_block(&self, height: u32) -> Result<FuelBlock> {
        let fuel_block = self
            .fuel_adapter
            .block_at_height(height)
            .await?
            .ok_or_else(|| {
                Error::Other(format!(
                    "Fuel node could not provide block at height: {height}"
                ))
            })?;

        self.block_validator
            .validate(fuel_block.id, &fuel_block.header, &fuel_block.consensus)?;
        Ok(fuel_block)
    }

    fn decide_action(
        &self,
        latest_submission: Option<&BlockSubmission>,
        current_epoch_block_height: u32,
    ) -> Action {
        let is_stale = latest_submission
            .map(|s| s.block_height < current_epoch_block_height)
            .unwrap_or(true);

        // if we reached the next epoch since our last submission, we should post the latest block
        if is_stale {
            return Action::Post;
        }

        match latest_submission {
            Some(submission) if !submission.completed => Action::UpdateTx {
                submission_id: submission.id.expect("submission to have id"),
                block_height: submission.block_height,
            },
            _ => Action::DoNothing,
        }
    }

    async fn update_transactions(
        &self,
        submission_id: NonNegative<i32>,
        block_height: u32,
    ) -> Result<()> {
        let transactions = self
            .storage
            .get_pending_block_submission_txs(submission_id)
            .await?;
        let current_block_number: u64 = self.l1_adapter.get_block_number().await?.into();

        for tx in transactions {
            let tx_hash = tx.hash;
            let Some(tx_response) = self.l1_adapter.get_transaction_response(tx_hash).await? else {
                continue; // not included
            };

            if !tx_response.succeeded() {
                self.storage
                    .update_block_submission_tx(tx_hash, TransactionState::Failed)
                    .await?;

                info!(
                    "failed submission for block: {block_height} with tx: {}",
                    hex::encode(tx_hash)
                );
                continue;
            }

            if !tx_response.confirmations(current_block_number) < self.num_blocks_to_finalize_tx {
                continue; // not finalized
            }

            self.storage
                .update_block_submission_tx(tx_hash, TransactionState::Finalized(self.clock.now()))
                .await?;

            info!(
                "finalized submission for block: {block_height} with tx: {}",
                hex::encode(tx_hash)
            );

            self.metrics
                .latest_committed_block
                .set(i64::from(block_height));
        }

        Ok(())
    }
}

impl<L1, Db, Fuel, BlockValidator, Clock> Runner
    for BlockCommitter<L1, Db, Fuel, BlockValidator, Clock>
where
    L1: ports::l1::Contract + ports::l1::Api,
    Db: Storage,
    Fuel: ports::fuel::Api,
    BlockValidator: Validator,
    Clock: ports::clock::Clock + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        let latest_submission = self.storage.submission_w_latest_block().await?;

        let current_block = self.fetch_latest_block().await?;
        let current_epoch_block_height =
            self.current_epoch_block_height(current_block.header.height);

        let action = self.decide_action(latest_submission.as_ref(), current_epoch_block_height);
        match action {
            Action::DoNothing => {}
            Action::Post => {
                let block = if current_block.header.height == current_epoch_block_height {
                    current_block
                } else {
                    self.fetch_block(current_epoch_block_height).await?
                };

                self.submit_block(block).await?;
            }
            Action::UpdateTx {
                submission_id,
                block_height,
            } => {
                self.update_transactions(submission_id, block_height)
                    .await?
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use clock::SystemClock;
    use fuel_crypto::{Message, SecretKey, Signature};
    use metrics::prometheus::{proto::Metric, Registry};
    use mockall::predicate::eq;
    use ports::{
        fuel::{FuelBlock, FuelBlockId, FuelConsensus, FuelHeader, FuelPoAConsensus},
        l1::{Contract, FragmentsSubmitted, MockContract},
        types::{BlockSubmissionTx, Fragment, L1Height, NonEmpty, TransactionResponse, Utc, U256},
    };
    use rand::{rngs::StdRng, Rng, SeedableRng};
    use storage::{DbWithProcess, PostgresProcess};

    use crate::{test_utils::mocks::l1::FullL1Mock, BlockValidator};

    use super::*;

    struct MockL1 {
        api: ports::l1::MockApi,
        contract: MockContract,
    }
    impl MockL1 {
        fn new() -> Self {
            Self {
                api: ports::l1::MockApi::new(),
                contract: MockContract::new(),
            }
        }
    }

    impl Contract for MockL1 {
        async fn submit(
            &self,
            hash: [u8; 32],
            height: u32,
        ) -> ports::l1::Result<BlockSubmissionTx> {
            self.contract.submit(hash, height).await
        }

        fn commit_interval(&self) -> NonZeroU32 {
            self.contract.commit_interval()
        }
    }

    impl ports::l1::Api for MockL1 {
        async fn submit_state_fragments(
            &self,
            _fragments: NonEmpty<Fragment>,
        ) -> ports::l1::Result<FragmentsSubmitted> {
            unimplemented!()
        }

        async fn get_block_number(&self) -> ports::l1::Result<L1Height> {
            self.api.get_block_number().await
        }

        async fn balance(&self, address: ports::types::Address) -> ports::l1::Result<U256> {
            self.api.balance(address).await
        }

        async fn get_transaction_response(
            &self,
            tx_hash: [u8; 32],
        ) -> ports::l1::Result<Option<TransactionResponse>> {
            self.api.get_transaction_response(tx_hash).await
        }
    }

    fn given_l1_that_expects_submission(block: FuelBlock) -> MockL1 {
        let mut l1 = MockL1::new();

        let submission_tx = BlockSubmissionTx {
            hash: [1u8; 32],
            ..Default::default()
        };
        l1.contract
            .expect_submit()
            .withf(move |hash, height| *hash == *block.id && *height == block.header.height)
            .return_once(move |_, _| Box::pin(async { Ok(submission_tx) }))
            .once();

        l1.api
            .expect_get_block_number()
            .return_once(move || Box::pin(async { Ok(0u32.into()) }));

        l1
    }

    fn given_l1_that_expects_transaction_response(
        block_number: u32,
        tx_hash: [u8; 32],
        response: Option<TransactionResponse>,
    ) -> MockL1 {
        let mut l1 = MockL1::new();

        l1.api
            .expect_get_block_number()
            .returning(move || Box::pin(async move { Ok(block_number.into()) }));

        l1.api
            .expect_get_transaction_response()
            .with(eq(tx_hash))
            .returning(move |_| {
                let response = response.clone();
                Box::pin(async move { Ok(response) })
            });

        l1.contract.expect_submit().never();

        l1
    }

    #[tokio::test]
    async fn will_do_nothing_if_latest_block_is_completed_and_not_stale() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let latest_block = given_a_block(10, &secret_key);
        let fuel_adapter = given_fetcher(vec![latest_block.clone()]);

        let db = db_with_submissions(vec![6, 8, 10]).await;
        db.update_block_submission_tx([10; 32], TransactionState::Finalized(Utc::now()))
            .await
            .unwrap();

        let mut l1 = MockL1::new();
        l1.contract.expect_submit().never();

        let mut block_committer = BlockCommitter::new(
            l1,
            db,
            fuel_adapter,
            block_validator,
            SystemClock,
            2.try_into().unwrap(),
            1,
        );

        // when
        block_committer.run().await.unwrap();

        // MockL1 verifies that submit was not called
    }

    #[tokio::test]
    async fn will_submit_on_latest_epoch() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let latest_block = given_a_block(10, &secret_key);
        let fuel_adapter = given_fetcher(vec![latest_block.clone()]);

        let l1 = given_l1_that_expects_submission(latest_block);
        let db = db_with_submissions(vec![]).await;
        let mut block_committer = BlockCommitter::new(
            l1,
            db,
            fuel_adapter,
            block_validator,
            SystemClock,
            2.try_into().unwrap(),
            1,
        );

        // when
        block_committer.run().await.unwrap();

        // MockL1 validates the expected calls are made
    }

    #[tokio::test]
    async fn will_skip_incomplete_submission_to_submit_latest() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let latest_block = given_a_block(10, &secret_key);
        let all_blocks = vec![
            given_a_block(8, &secret_key),
            given_a_block(9, &secret_key),
            latest_block.clone(),
        ];
        let fuel_adapter = given_fetcher(all_blocks);

        let l1 = given_l1_that_expects_submission(latest_block);
        let db = db_with_submissions(vec![8]).await;

        let mut block_committer = BlockCommitter::new(
            l1,
            db,
            fuel_adapter,
            block_validator,
            SystemClock,
            2.try_into().unwrap(),
            1,
        );

        // when
        block_committer.run().await.unwrap();

        // MockL1 validates the expected calls are made
    }

    #[tokio::test]
    async fn will_fetch_and_submit_missed_block() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let missed_block = given_a_block(4, &secret_key);
        let latest_block = given_a_block(5, &secret_key);
        let fuel_adapter = given_fetcher(vec![latest_block, missed_block.clone()]);

        let l1 = given_l1_that_expects_submission(missed_block);
        let db = db_with_submissions(vec![0, 2]).await;
        let mut block_committer = BlockCommitter::new(
            l1,
            db,
            fuel_adapter,
            block_validator,
            SystemClock,
            2.try_into().unwrap(),
            1,
        );

        // when
        block_committer.run().await.unwrap();

        // then
        // MockL1 validates the expected calls are made
    }

    #[tokio::test]
    async fn will_not_reattempt_submitting_missed_block() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let missed_block = given_a_block(4, &secret_key);
        let latest_block = given_a_block(5, &secret_key);
        let fuel_adapter = given_fetcher(vec![latest_block, missed_block]);

        let db = db_with_submissions(vec![0, 2, 4]).await;

        let l1 = given_l1_that_expects_transaction_response(5, [4; 32], None);

        let mut block_committer = BlockCommitter::new(
            l1,
            db,
            fuel_adapter,
            block_validator,
            SystemClock,
            2.try_into().unwrap(),
            1,
        );

        // when
        block_committer.run().await.unwrap();

        // then
        // Mock verifies that the submit didn't happen
    }

    #[tokio::test]
    async fn propagates_block_if_epoch_reached() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let block = given_a_block(4, &secret_key);
        let fuel_adapter = given_fetcher(vec![block.clone()]);

        let db = db_with_submissions(vec![0, 2]).await;
        let l1 = given_l1_that_expects_submission(block);
        let mut block_committer = BlockCommitter::new(
            l1,
            db,
            fuel_adapter,
            block_validator,
            SystemClock,
            2.try_into().unwrap(),
            1,
        );

        // when
        block_committer.run().await.unwrap();

        // then
        // Mock verifies that submit was called with the appropriate block
    }

    #[tokio::test]
    async fn updates_block_metric_regardless_if_block_is_published() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let block = given_a_block(5, &secret_key);
        let fuel_adapter = given_fetcher(vec![block]);

        let db = db_with_submissions(vec![0, 2, 4]).await;

        let mut l1 = FullL1Mock::default();
        l1.contract.expect_submit().never();
        l1.api
            .expect_get_block_number()
            .returning(|| Box::pin(async { Ok(4u32.into()) }));
        l1.api
            .expect_get_transaction_response()
            .returning(|_| Box::pin(async { Ok(None) }));

        let mut block_committer = BlockCommitter::new(
            l1,
            db,
            fuel_adapter,
            block_validator,
            SystemClock,
            2.try_into().unwrap(),
            1,
        );

        let registry = Registry::default();
        block_committer.register_metrics(&registry);

        // when
        block_committer.run().await.unwrap();

        // then
        let metrics = registry.gather();
        let latest_block_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "latest_fuel_block")
            .and_then(|metric| metric.get_metric().first())
            .map(Metric::get_gauge)
            .unwrap();

        assert_eq!(latest_block_metric.get_value(), 5f64);
    }

    #[tokio::test]
    async fn updates_latest_committed_block_metric() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let latest_height = 4;
        let latest_block = given_a_block(latest_height, &secret_key);
        let fuel_adapter = given_fetcher(vec![latest_block]);

        let db = db_with_submissions(vec![0, 2, 4]).await;
        let tx_response = TransactionResponse::new(latest_height as u64, true);
        let l1 =
            given_l1_that_expects_transaction_response(latest_height, [4; 32], Some(tx_response));

        let mut block_committer = BlockCommitter::new(
            l1,
            db,
            fuel_adapter,
            block_validator,
            SystemClock,
            2.try_into().unwrap(),
            1,
        );

        let registry = Registry::default();
        block_committer.register_metrics(&registry);

        // when
        block_committer.run().await.unwrap();

        // then
        let metrics = registry.gather();
        let latest_committed_block_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "latest_committed_block")
            .and_then(|metric| metric.get_metric().first())
            .map(Metric::get_gauge)
            .unwrap();

        assert_eq!(
            latest_committed_block_metric.get_value(),
            f64::from(latest_height)
        );
    }

    #[tokio::test]
    async fn updates_submission_state_to_finalized() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let latest_height = 4;
        let latest_block = given_a_block(latest_height, &secret_key);
        let fuel_adapter = given_fetcher(vec![latest_block]);

        let db = db_with_submissions(vec![0, 2, 4]).await;
        let tx_response = TransactionResponse::new(latest_height as u64, true);
        let l1 =
            given_l1_that_expects_transaction_response(latest_height, [4; 32], Some(tx_response));

        let mut block_committer = BlockCommitter::new(
            l1,
            db,
            fuel_adapter,
            block_validator,
            SystemClock,
            2.try_into().unwrap(),
            1,
        );

        // when
        block_committer.run().await.unwrap();

        // then
        let latest_submission = block_committer
            .storage
            .submission_w_latest_block()
            .await
            .unwrap()
            .expect("submission to exist");
        let pending_txs = block_committer
            .storage
            .get_pending_block_submission_txs(latest_submission.id.expect("submission to have id"))
            .await
            .unwrap();

        assert!(pending_txs.is_empty());
    }

    async fn db_with_submissions(pending_submissions: Vec<u32>) -> DbWithProcess {
        let db = PostgresProcess::shared()
            .await
            .unwrap()
            .create_random_db()
            .await
            .unwrap();
        for height in pending_submissions {
            let submission_tx = BlockSubmissionTx {
                hash: [height as u8; 32],
                nonce: height,
                ..Default::default()
            };
            db.record_block_submission(submission_tx, given_incomplete_submission(height))
                .await
                .unwrap();
        }

        db
    }

    fn given_fetcher(available_blocks: Vec<FuelBlock>) -> ports::fuel::MockApi {
        let mut fetcher = ports::fuel::MockApi::new();
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

    fn given_incomplete_submission(block_height: u32) -> BlockSubmission {
        let mut submission: BlockSubmission = rand::thread_rng().gen();
        submission.block_height = block_height;
        submission.completed = false;

        submission
    }

    fn given_a_block(height: u32, secret_key: &SecretKey) -> FuelBlock {
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
            transactions: vec![],
            block_producer: Some(secret_key.public_key()),
        }
    }

    fn given_header(height: u32) -> FuelHeader {
        let application_hash = "0x017ab4b70ea129c29e932d44baddc185ad136bf719c4ada63a10b5bf796af91e"
            .parse()
            .unwrap();

        FuelHeader {
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

    fn given_secret_key() -> SecretKey {
        let mut rng = StdRng::seed_from_u64(42);

        SecretKey::random(&mut rng)
    }
}
