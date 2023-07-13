use std::{num::NonZeroU32, vec};

use async_trait::async_trait;
use prometheus::{core::Collector, IntGauge, Opts};
use tokio::sync::mpsc::Sender;

use crate::{
    adapters::{
        block_fetcher::{FuelAdapter, FuelBlock},
        runner::Runner,
        storage::{BlockSubmission, Storage},
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
        block_fetcher: impl FuelAdapter + 'static,
        storage: impl Storage + 'static,
    ) -> Self {
        Self {
            commit_interval,
            fuel_adapter: Box::new(block_fetcher),
            tx_fuel_block,
            storage: Box::new(storage),
            metrics: Default::default(),
        }
    }

    async fn fetch_latest_block(&self) -> Result<FuelBlock> {
        let current_block = self.fuel_adapter.latest_block().await?;

        self.metrics
            .latest_fuel_block
            .set(current_block.height as i64);

        Ok(current_block)
    }

    fn block_not_stale(
        current_block_height: u32,
        height_of_latest_submitted_block: Option<u32>,
    ) -> bool {
        !height_of_latest_submitted_block
            .is_some_and(|submitted_height| current_block_height <= submitted_height)
    }

    fn is_epoch_reached(current_block_height: u32, commit_epoch: NonZeroU32) -> bool {
        current_block_height % commit_epoch == 0
    }

    fn should_propagate_update(
        commit_epoch: NonZeroU32,
        current_block_height: u32,
        height_of_latest_submitted_block: Option<u32>,
    ) -> bool {
        Self::block_not_stale(current_block_height, height_of_latest_submitted_block)
            && Self::is_epoch_reached(current_block_height, commit_epoch)
    }

    fn expected_block_height(commit_interval: NonZeroU32, current_block_height: u32) -> u32 {
        current_block_height - (current_block_height % commit_interval)
    }

    fn should_commit_previous_epoch_block(
        commit_interval: NonZeroU32,
        current_block_height: u32,
        height_of_latest_submitted_block: Option<u32>,
    ) -> bool {
        let Some(submission_height) = height_of_latest_submitted_block else {
            return true;
        };

        submission_height < Self::expected_block_height(commit_interval, current_block_height)
    }
}

#[async_trait]
impl Runner for BlockWatcher {
    async fn run(&mut self) -> Result<()> {
        let current_block = self.fetch_latest_block().await?;

        let latest_block_submission = self
            .storage
            .submission_w_latest_block()
            .await?
            .map(|submission| submission.block.height);

        let block = if Self::should_propagate_update(
            self.commit_interval,
            current_block.height,
            latest_block_submission,
        ) {
            current_block
        } else if Self::should_commit_previous_epoch_block(
            self.commit_interval,
            current_block.height,
            latest_block_submission,
        ) {
            let last_epoch_block_height =
                Self::expected_block_height(self.commit_interval, current_block.height);
            self.fuel_adapter
                .block_at_height(last_epoch_block_height)
                .await?
                .ok_or_else(|| {
                    Error::Other(format!(
                        "Fuel node could not provide block at height: {last_epoch_block_height}"
                    ))
                })?
        } else {
            return Ok(())
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

    use mockall::predicate::eq;
    use prometheus::Registry;

    use super::*;
    use crate::adapters::{
        block_fetcher::{FuelAdapter, MockFuelAdapter},
        storage::{sqlite_db::SqliteDb, BlockSubmission},
    };

    #[tokio::test]
    async fn will_fetch_and_propagate_missed_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let latest_block = given_a_block(5);
        let missed_block = given_a_block(4);

        let block_fetcher = given_fetcher(vec![latest_block, missed_block]);

        let storage = SqliteDb::temporary().await.unwrap();
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, block_fetcher, storage);

        // when
        block_watcher.run().await.unwrap();

        //then
        let Ok(announced_block) = rx.try_recv() else {
            panic!("Didn't receive the block")
        };

        assert_eq!(missed_block, announced_block);
    }

    #[tokio::test]
    async fn will_propagate_a_received_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let block = given_a_block(4);

        let block_fetcher = given_fetcher(vec![block]);

        let storage = SqliteDb::temporary().await.unwrap();
        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, block_fetcher, storage);

        // when
        block_watcher.run().await.unwrap();

        //then
        let Ok(announced_block) = rx.try_recv() else {
            panic!("Didn't receive the block")
        };

        assert_eq!(block, announced_block);
    }

    #[tokio::test]
    async fn will_not_propagate_a_stale_block() {
        let height_of_latest_submitted_block = 2;

        {
            let current_block_height = 1;

            let should_propagate = BlockWatcher::should_propagate_update(
                1.try_into().unwrap(),
                current_block_height,
                Some(height_of_latest_submitted_block),
            );

            assert!(!should_propagate);
        }
        {
            let current_block_height = 2;

            let should_propagate = BlockWatcher::should_propagate_update(
                1.try_into().unwrap(),
                current_block_height,
                Some(height_of_latest_submitted_block),
            );

            assert!(!should_propagate);
        }
    }

    #[tokio::test]
    async fn will_propagate_even_if_last_tx_is_pending() {
        let current_block_height = 2;
        let height_of_latest_submitted_block = 1;

        let should_propagate = BlockWatcher::should_propagate_update(
            1.try_into().unwrap(),
            current_block_height,
            Some(height_of_latest_submitted_block),
        );

        assert!(should_propagate);
    }

    #[tokio::test]
    async fn respects_epoch_when_posting_block_updates() {
        let commit_epoch = NonZeroU32::new(3).unwrap();

        let check_should_submit = |block_height, should_submit| {
            let actual = BlockWatcher::should_propagate_update(commit_epoch, block_height, None);

            assert_eq!(actual, should_submit);
        };

        check_should_submit(2, false);
        check_should_submit(3, true);
        check_should_submit(4, false);
    }

    #[tokio::test]
    async fn updates_block_metric_regardless_if_block_is_published() {
        // given
        let (tx, _) = tokio::sync::mpsc::channel(10);

        let block_fetcher = given_fetcher(vec![given_a_block(5)]);

        let storage = SqliteDb::temporary().await.unwrap();
        storage.insert(given_a_pending_submission(4)).await.unwrap();

        let mut block_watcher =
            BlockWatcher::new(2.try_into().unwrap(), tx, block_fetcher, storage);

        let registry = Registry::default();
        block_watcher.register_metrics(&registry);

        // when
        block_watcher.run().await.unwrap();

        //then
        let metrics = registry.gather();
        let latest_block_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "latest_fuel_block")
            .and_then(|metric| metric.get_metric().get(0))
            .map(|metric| metric.get_gauge())
            .unwrap();

        assert_eq!(latest_block_metric.get_value(), 5f64);
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
        BlockSubmission {
            block: FuelBlock {
                hash: Default::default(),
                height: block_height,
            },
            ..BlockSubmission::random()
        }
    }

    fn given_a_block(block_height: u32) -> FuelBlock {
        FuelBlock {
            hash: Default::default(),
            height: block_height,
        }
    }
}
