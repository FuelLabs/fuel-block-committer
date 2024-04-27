use std::{num::NonZeroU32, vec};

use async_trait::async_trait;
use prometheus::{core::Collector, IntGauge, Opts};
use tokio::sync::mpsc::Sender;

use crate::{
    adapters::{
        fuel_adapter::{FuelAdapter, FuelBlock},
        runner::Runner,
        storage::Storage,
    },
    errors::{Error, Result},
    telemetry::RegistersMetrics,
};

struct Metrics {
    latest_fuel_block: IntGauge,
}

impl RegistersMetrics for BlockWatcher {
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

pub struct BlockWatcher {
    fuel_adapter: Box<dyn FuelAdapter>,
    tx_fuel_block: Sender<FuelBlock>,
    storage: Box<dyn Storage>,
    commit_interval: NonZeroU32,
    metrics: Metrics,
}

impl BlockWatcher {
    pub fn new(
        commit_interval: NonZeroU32,
        tx_fuel_block: Sender<FuelBlock>,
        fuel_adapter: impl FuelAdapter + 'static,
        storage: impl Storage + 'static,
    ) -> Self {
        Self {
            commit_interval,
            fuel_adapter: Box::new(fuel_adapter),
            tx_fuel_block,
            storage: Box::new(storage),
            metrics: Metrics::default(),
        }
    }

    async fn fetch_latest_block(&self) -> Result<FuelBlock> {
        let current_block = self.fuel_adapter.latest_block().await?;

        self.metrics
            .latest_fuel_block
            .set(i64::from(current_block.height));

        Ok(current_block)
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
            .map(|submission| submission.block.height))
    }

    async fn fetch_block(&self, height: u32) -> Result<FuelBlock> {
        self.fuel_adapter
            .block_at_height(height)
            .await?
            .ok_or_else(|| {
                Error::Other(format!(
                    "Fuel node could not provide block at height: {height}"
                ))
            })
    }
}

#[async_trait]
impl Runner for BlockWatcher {
    async fn run(&mut self) -> Result<()> {
        let current_block = self.fetch_latest_block().await?;
        let current_epoch_block_height = self.current_epoch_block_height(current_block.height);

        if self.check_if_stale(current_epoch_block_height).await? {
            return Ok(());
        }

        let block = if current_block.height == current_epoch_block_height {
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
    use std::vec;

    use mockall::predicate::eq;
    use prometheus::{proto::Metric, Registry};

    use super::*;
    use crate::adapters::{
        fuel_adapter::MockFuelAdapter,
        storage::{postgresql::PostgresProcess, BlockSubmission},
    };

    #[tokio::test]
    async fn will_fetch_and_propagate_missed_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let missed_block = given_a_block(4);
        let latest_block = given_a_block(5);
        let fuel_adapter = given_fetcher(vec![latest_block, missed_block]);

        let process = start_postgress_with_submissions(vec![0, 2]).await;
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, fuel_adapter, process.db());

        // when
        block_watcher.run().await.unwrap();

        //then
        let Ok(announced_block) = rx.try_recv() else {
            panic!("Didn't receive the block")
        };

        assert_eq!(missed_block, announced_block);
    }

    #[tokio::test]
    async fn will_not_reattempt_commiting_missed_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let missed_block = given_a_block(4);
        let latest_block = given_a_block(5);
        let fuel_adapter = given_fetcher(vec![latest_block, missed_block]);

        let process = start_postgress_with_submissions(vec![0, 2, 4]).await;
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, fuel_adapter, process.db());

        // when
        block_watcher.run().await.unwrap();

        //then
        if let Ok(block) = rx.try_recv() {
            panic!("Should not have received a block. Block: {block:?}");
        }
    }

    #[tokio::test]
    async fn will_not_reattempt_commiting_latest_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let latest_block = given_a_block(6);
        let fuel_adapter = given_fetcher(vec![latest_block]);

        let process = start_postgress_with_submissions(vec![0, 2, 4, 6]).await;
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, fuel_adapter, process.db());

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

        let block = given_a_block(4);
        let fuel_adapter = given_fetcher(vec![block]);

        let process = start_postgress_with_submissions(vec![0, 2]).await;
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, fuel_adapter, process.db());

        // when
        block_watcher.run().await.unwrap();

        //then
        let Ok(announced_block) = rx.try_recv() else {
            panic!("Didn't receive the block")
        };

        assert_eq!(block, announced_block);
    }

    #[tokio::test]
    async fn updates_block_metric_regardless_if_block_is_published() {
        // given
        let (tx, _) = tokio::sync::mpsc::channel(10);

        let fuel_adapter = given_fetcher(vec![given_a_block(5)]);

        let process = start_postgress_with_submissions(vec![0, 2, 4]).await;
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, fuel_adapter, process.db());

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

    async fn start_postgress_with_submissions(pending_submissions: Vec<u32>) -> PostgresProcess {
        let process = PostgresProcess::start().await.unwrap();

        let storage = process.db();
        for height in pending_submissions {
            storage
                .insert(given_a_pending_submission(height))
                .await
                .unwrap();
        }

        process
    }

    fn given_fetcher(available_blocks: Vec<FuelBlock>) -> MockFuelAdapter {
        let mut fetcher = MockFuelAdapter::new();
        for block in available_blocks.clone() {
            fetcher
                .expect_block_at_height()
                .with(eq(block.height))
                .returning(move |_| Ok(Some(block)));
        }
        if let Some(block) = available_blocks.into_iter().max_by_key(|el| el.height) {
            fetcher.expect_latest_block().returning(move || Ok(block));
        }

        fetcher
    }

    fn given_a_pending_submission(block_height: u32) -> BlockSubmission {
        let mut submission = BlockSubmission::random();
        submission.block.height = block_height;
        submission
    }

    fn given_a_block(block_height: u32) -> FuelBlock {
        FuelBlock {
            hash: Default::default(),
            height: block_height,
        }
    }
}
