use std::num::NonZeroU32;

use async_trait::async_trait;
use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use ports::{
    storage::Storage,
    types::{BlockSubmission, ValidatedFuelBlock},
};
use tracing::info;
use validator::Validator;

use super::Runner;
use crate::{Error, Result};

pub struct BlockCommitter<L1, Db, Fuel, BlockValidator> {
    l1_adapter: L1,
    fuel_adapter: Fuel,
    storage: Db,
    block_validator: BlockValidator,
    commit_interval: NonZeroU32,
    metrics: Metrics,
}

struct Metrics {
    latest_fuel_block: IntGauge,
}

impl<L1, Db, Fuel, BlockValidator> RegistersMetrics
    for BlockCommitter<L1, Db, Fuel, BlockValidator>
{
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![Box::new(self.metrics.latest_fuel_block.clone())]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let latest_fuel_block = IntGauge::with_opts(Opts::new(
            "latest_fuel_block",
            "The height of the latest fuel block.",
        ))
        .expect("fuel_network_errors metric to be correctly configured");

        Self { latest_fuel_block }
    }
}

impl<L1, Db, Fuel, BlockValidator> BlockCommitter<L1, Db, Fuel, BlockValidator> {
    pub fn new(
        l1: L1,
        storage: Db,
        fuel_adapter: Fuel,
        block_validator: BlockValidator,
        commit_interval: NonZeroU32,
    ) -> Self {
        Self {
            l1_adapter: l1,
            storage,
            fuel_adapter,
            block_validator,
            commit_interval,
            metrics: Metrics::default(),
        }
    }
}

impl<L1, Db, Fuel, BlockValidator> BlockCommitter<L1, Db, Fuel, BlockValidator>
where
    L1: ports::l1::Contract + ports::l1::Api,
    Db: Storage,
    BlockValidator: Validator,
    Fuel: ports::fuel::Api,
{
    async fn submit_block(&self, fuel_block: ValidatedFuelBlock) -> Result<()> {
        let submittal_height = self.l1_adapter.get_block_number().await?;

        let submission = BlockSubmission {
            block_hash: fuel_block.hash(),
            block_height: fuel_block.height(),
            submittal_height,
            completed: false,
        };

        self.storage.insert(submission).await?;

        // if we have a network failure the DB entry will be left at completed:false.
        self.l1_adapter.submit(fuel_block).await?;

        Ok(())
    }

    async fn fetch_latest_block(&self) -> Result<ValidatedFuelBlock> {
        let latest_block = self.fuel_adapter.latest_block().await?;
        let validated_block = self.block_validator.validate(&latest_block)?;

        self.metrics
            .latest_fuel_block
            .set(i64::from(validated_block.height()));

        Ok(validated_block)
    }

    async fn check_if_stale(&self, block_height: u32) -> Result<bool> {
        let Some(submitted_height) = self.last_submitted_block_height().await? else {
            return Ok(false);
        };

        Ok(submitted_height >= block_height)
    }

    fn current_epoch_block_height(&self, current_block_height: u32) -> u32 {
        current_block_height - (current_block_height % self.commit_interval)
    }

    async fn last_submitted_block_height(&self) -> Result<Option<u32>> {
        Ok(self
            .storage
            .submission_w_latest_block()
            .await?
            .map(|submission| submission.block_height))
    }

    async fn fetch_block(&self, height: u32) -> Result<ValidatedFuelBlock> {
        let fuel_block = self
            .fuel_adapter
            .block_at_height(height)
            .await?
            .ok_or_else(|| {
                Error::Other(format!(
                    "Fuel node could not provide block at height: {height}"
                ))
            })?;

        Ok(self.block_validator.validate(&fuel_block)?)
    }
}

#[async_trait]
impl<L1, Db, Fuel, BlockValidator> Runner for BlockCommitter<L1, Db, Fuel, BlockValidator>
where
    L1: ports::l1::Contract + ports::l1::Api,
    Db: Storage,
    Fuel: ports::fuel::Api,
    BlockValidator: Validator,
{
    async fn run(&mut self) -> Result<()> {
        let current_block = self.fetch_latest_block().await?;
        let current_epoch_block_height = self.current_epoch_block_height(current_block.height());

        if self.check_if_stale(current_epoch_block_height).await? {
            return Ok(());
        }

        let block = if current_block.height() == current_epoch_block_height {
            current_block
        } else {
            self.fetch_block(current_epoch_block_height).await?
        };

        self.submit_block(block)
            .await
            .map_err(|e| Error::Other(e.to_string()))?;
        info!("submitted {block:?}!");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fuel_crypto::{Message, SecretKey, Signature};
    use metrics::prometheus::{proto::Metric, Registry};
    use mockall::predicate::{self, eq};
    use ports::{
        fuel::{FuelBlock, FuelBlockId, FuelConsensus, FuelHeader, FuelPoAConsensus},
        l1::{Contract, EventStreamer, GasPrices, GasUsage, MockContract},
        types::{L1Height, NonEmptyVec, TransactionResponse, U256},
    };
    use rand::{rngs::StdRng, Rng, SeedableRng};
    use storage::{DbWithProcess, Postgres, PostgresProcess};
    use validator::BlockValidator;

    use crate::test_utils::mocks::l1::FullL1Mock;

    use super::*;

    fn given_l1_that_expects_submission(block: ValidatedFuelBlock) -> FullL1Mock {
        let mut l1 = FullL1Mock::default();

        l1.contract
            .expect_submit()
            .with(predicate::eq(block))
            .return_once(move |_| Ok(()));

        l1.api
            .expect_get_block_number()
            .return_once(move || Ok(0u32.into()));

        l1
    }

    #[tokio::test]
    async fn will_fetch_and_submit_missed_block() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let missed_block = given_a_block(4, &secret_key);
        let latest_block = given_a_block(5, &secret_key);
        let fuel_adapter = given_fetcher(vec![latest_block, missed_block.clone()]);

        let validated_missed_block = ValidatedFuelBlock::new(*missed_block.id, 4);
        let l1 = given_l1_that_expects_submission(validated_missed_block);
        let db = db_with_submissions(vec![0, 2]).await;
        let mut block_committer =
            BlockCommitter::new(l1, db, fuel_adapter, block_validator, 2.try_into().unwrap());

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

        let mut l1 = FullL1Mock::default();
        l1.contract.expect_submit().never();

        let mut block_committer =
            BlockCommitter::new(l1, db, fuel_adapter, block_validator, 2.try_into().unwrap());

        // when
        block_committer.run().await.unwrap();

        // then
        // Mock verifies that the submit didn't happen
    }

    #[tokio::test]
    async fn will_not_reattempt_committing_latest_block() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let latest_block = given_a_block(6, &secret_key);
        let fuel_adapter = given_fetcher(vec![latest_block]);

        let db = db_with_submissions(vec![0, 2, 4, 6]).await;

        let mut l1 = FullL1Mock::default();
        l1.contract.expect_submit().never();

        let mut block_committer =
            BlockCommitter::new(l1, db, fuel_adapter, block_validator, 2.try_into().unwrap());

        // when
        block_committer.run().await.unwrap();

        // then
        // MockL1 verifies that submit was not called
    }

    #[tokio::test]
    async fn propagates_block_if_epoch_reached() {
        // given
        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(*secret_key.public_key().hash());
        let block = given_a_block(4, &secret_key);
        let fuel_adapter = given_fetcher(vec![block.clone()]);

        let db = db_with_submissions(vec![0, 2]).await;
        let l1 = given_l1_that_expects_submission(ValidatedFuelBlock::new(*block.id, 4));
        let mut block_committer =
            BlockCommitter::new(l1, db, fuel_adapter, block_validator, 2.try_into().unwrap());

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

        let mut block_committer =
            BlockCommitter::new(l1, db, fuel_adapter, block_validator, 2.try_into().unwrap());

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

    async fn db_with_submissions(pending_submissions: Vec<u32>) -> DbWithProcess {
        let db = PostgresProcess::shared()
            .await
            .unwrap()
            .create_random_db()
            .await
            .unwrap();
        for height in pending_submissions {
            db.insert(given_a_pending_submission(height)).await.unwrap();
        }

        db
    }

    fn given_fetcher(available_blocks: Vec<FuelBlock>) -> ports::fuel::MockApi {
        let mut fetcher = ports::fuel::MockApi::new();
        for block in available_blocks.clone() {
            fetcher
                .expect_block_at_height()
                .with(eq(block.header.height))
                .returning(move |_| Ok(Some(block.clone())));
        }
        if let Some(block) = available_blocks
            .into_iter()
            .max_by_key(|el| el.header.height)
        {
            fetcher
                .expect_latest_block()
                .returning(move || Ok(block.clone()));
        }

        fetcher
    }

    fn given_a_pending_submission(block_height: u32) -> BlockSubmission {
        let mut submission: BlockSubmission = rand::thread_rng().gen();
        submission.block_height = block_height;

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
