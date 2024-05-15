use std::{num::NonZeroU32, vec};

use async_trait::async_trait;
use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use ports::{storage::Storage, types::ValidatedFuelBlock};
use tokio::sync::mpsc::Sender;
use validator::Validator;

use super::Runner;
use crate::{Error, Result};

struct Metrics {
    latest_fuel_block: IntGauge,
}

impl<A, Db, V> RegistersMetrics for BlockWatcher<A, Db, V> {
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

pub struct BlockWatcher<A, Db, V> {
    fuel_adapter: A,
    tx_fuel_block: Sender<ValidatedFuelBlock>,
    storage: Db,
    block_validator: V,
    commit_interval: NonZeroU32,
    metrics: Metrics,
}

impl<A, Db, V> BlockWatcher<A, Db, V> {
    pub fn new(
        commit_interval: NonZeroU32,
        tx_fuel_block: Sender<ValidatedFuelBlock>,
        fuel_adapter: A,
        storage: Db,
        block_validator: V,
    ) -> Self {
        Self {
            commit_interval,
            fuel_adapter,
            tx_fuel_block,
            storage,
            block_validator,
            metrics: Metrics::default(),
        }
    }
}

impl<A, Db, V> BlockWatcher<A, Db, V>
where
    A: ports::fuel::Api,
    Db: Storage,
    V: Validator,
{
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
impl<A, Db, V> Runner for BlockWatcher<A, Db, V>
where
    A: ports::fuel::Api,
    Db: Storage,
    V: Validator,
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

        self.tx_fuel_block
            .send(block)
            .await
            .map_err(|e| Error::Other(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, vec};

    use fuel_crypto::{Message, SecretKey, Signature};
    use metrics::prometheus::{proto::Metric, Registry};
    use mockall::predicate::eq;
    use ports::{
        fuel::{FuelBlock, FuelBlockId, FuelConsensus, FuelHeader, FuelPoAConsensus, MockApi},
        types::BlockSubmission,
    };
    use rand::{rngs::StdRng, Rng, SeedableRng};
    use storage::{Postgres, PostgresProcess};
    use validator::BlockValidator;

    use super::*;

    #[tokio::test]
    async fn will_fetch_and_propagate_missed_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(secret_key.public_key());
        let missed_block = given_a_block(4, &secret_key);
        let latest_block = given_a_block(5, &secret_key);
        let fuel_adapter = given_fetcher(vec![latest_block, missed_block.clone()]);

        let process = PostgresProcess::shared().await.unwrap();
        let db = db_with_submissions(&process, vec![0, 2]).await;
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, fuel_adapter, db, block_validator);

        // when
        block_watcher.run().await.unwrap();

        //then
        let Ok(announced_block) = rx.try_recv() else {
            panic!("Didn't receive the block")
        };

        assert_eq!(announced_block, missed_block.into());
    }

    #[tokio::test]
    async fn will_not_reattempt_committing_missed_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(secret_key.public_key());
        let missed_block = given_a_block(4, &secret_key);
        let latest_block = given_a_block(5, &secret_key);
        let fuel_adapter = given_fetcher(vec![latest_block, missed_block]);

        let process = PostgresProcess::shared().await.unwrap();
        let db = db_with_submissions(&process, vec![0, 2, 4]).await;
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, fuel_adapter, db, block_validator);

        // when
        block_watcher.run().await.unwrap();

        //then
        if let Ok(block) = rx.try_recv() {
            panic!("Should not have received a block. Block: {block:?}");
        }
    }

    #[tokio::test]
    async fn will_not_reattempt_committing_latest_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(secret_key.public_key());
        let latest_block = given_a_block(6, &secret_key);
        let fuel_adapter = given_fetcher(vec![latest_block]);

        let process = PostgresProcess::shared().await.unwrap();
        let db = db_with_submissions(&process, vec![0, 2, 4, 6]).await;
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, fuel_adapter, db, block_validator);

        // when
        block_watcher.run().await.unwrap();

        //then
        if let Ok(block) = rx.try_recv() {
            panic!("Should not have received a block. Block: {block:?}");
        }
    }

    #[tokio::test]
    async fn propagates_block_if_epoch_reached() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(secret_key.public_key());
        let block = given_a_block(4, &secret_key);
        let fuel_adapter = given_fetcher(vec![block.clone()]);

        let process = PostgresProcess::shared().await.unwrap();
        let db = db_with_submissions(&process, vec![0, 2]).await;
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, fuel_adapter, db, block_validator);

        // when
        block_watcher.run().await.unwrap();

        //then
        let Ok(announced_block) = rx.try_recv() else {
            panic!("Didn't receive the block")
        };

        assert_eq!(announced_block, block.into());
    }

    #[tokio::test]
    async fn updates_block_metric_regardless_if_block_is_published() {
        // given
        let (tx, _) = tokio::sync::mpsc::channel(10);

        let secret_key = given_secret_key();
        let block_validator = BlockValidator::new(secret_key.public_key());
        let block = given_a_block(5, &secret_key);
        let fuel_adapter = given_fetcher(vec![block]);

        let process = PostgresProcess::shared().await.unwrap();
        let db = db_with_submissions(&process, vec![0, 2, 4]).await;
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, fuel_adapter, db, block_validator);

        let registry = Registry::default();
        block_watcher.register_metrics(&registry);

        // when
        block_watcher.run().await.unwrap();

        //then
        let metrics = registry.gather();
        let latest_block_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "latest_fuel_block")
            .and_then(|metric| metric.get_metric().first())
            .map(Metric::get_gauge)
            .unwrap();

        assert_eq!(latest_block_metric.get_value(), 5f64);
    }

    async fn db_with_submissions(
        process: &Arc<PostgresProcess>,
        pending_submissions: Vec<u32>,
    ) -> Postgres {
        let db = process.create_random_db().await.unwrap();
        for height in pending_submissions {
            db.insert(given_a_pending_submission(height)).await.unwrap();
        }

        db
    }

    fn given_fetcher(available_blocks: Vec<FuelBlock>) -> MockApi {
        let mut fetcher = MockApi::new();
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
