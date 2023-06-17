use std::vec;

use async_trait::async_trait;
use prometheus::{core::Collector, IntGauge, Opts};
use tokio::sync::mpsc::Sender;

use crate::{
    adapters::{
        block_fetcher::{BlockFetcher, FuelBlock},
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
    block_fetcher: Box<dyn BlockFetcher>,
    tx_fuel_block: Sender<FuelBlock>,
    storage: Box<dyn Storage>,
    commit_epoch: u32,
    metrics: Metrics,
}

impl BlockWatcher {
    pub fn new(
        commit_epoch: u32,
        tx_fuel_block: Sender<FuelBlock>,
        block_fetcher: impl BlockFetcher + 'static,
        storage: impl Storage + 'static,
    ) -> Self {
        Self {
            commit_epoch,
            block_fetcher: Box::new(block_fetcher),
            tx_fuel_block,
            storage: Box::new(storage),
            metrics: Default::default(),
        }
    }

    async fn fetch_latest_block(&self) -> Result<FuelBlock> {
        let current_block = self.block_fetcher.latest_block().await?;

        self.metrics
            .latest_fuel_block
            .set(current_block.height as i64);

        Ok(current_block)
    }

    fn block_not_stale(
        current_block: &FuelBlock,
        last_block_submission: Option<&BlockSubmission>,
    ) -> bool {
        !last_block_submission
            .is_some_and(|submission| current_block.height <= submission.block.height)
    }

    fn is_epoch_reached(current_block: &FuelBlock, commit_epoch: u32) -> bool {
        current_block.height % commit_epoch == 0
    }

    fn should_propagate_update(
        commit_epoch: u32,
        current_block: &FuelBlock,
        last_block_submission: Option<&BlockSubmission>,
    ) -> bool {
        Self::block_not_stale(current_block, last_block_submission)
            && Self::is_epoch_reached(current_block, commit_epoch)
    }
}

#[async_trait]
impl Runner for BlockWatcher {
    async fn run(&mut self) -> Result<()> {
        let current_block = self.fetch_latest_block().await?;

        let latest_block_submission = self.storage.submission_w_latest_block().await?;

        if Self::should_propagate_update(
            self.commit_epoch,
            &current_block,
            latest_block_submission.as_ref(),
        ) {
            self.tx_fuel_block
                .send(current_block)
                .await
                .map_err(|e| Error::Other(e.to_string()))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, vec};

    use prometheus::Registry;

    use super::*;
    use crate::adapters::{
        block_fetcher::MockBlockFetcher,
        storage::{sqlite_db::SqliteDb, BlockSubmission},
    };

    // #[tokio::test]
    // async fn will_propagate_a_received_block() {
    //     // given
    //     let (tx, mut rx) = tokio::sync::mpsc::channel(10);
    //
    //     let block = given_a_block(5);
    //
    //     let block_fetcher = given_fetcher_that_returns(vec![block.clone()]);
    //
    //     let storage = SqliteDb::temporary().await.unwrap();
    //     storage
    //         .insert(BlockSubmission {
    //             fuel_block_hash: Default::default(),
    //             fuel_block_height: 3,
    //             completed: true,
    //             tx_hash: H256::default(),
    //         })
    //         .await
    //         .unwrap();
    //     let block_watcher = BlockWatcher::new(2, tx, block_fetcher, storage);
    //
    //     // when
    //     block_watcher.run().await.unwrap();
    //
    //     //then
    //     let Ok(announced_block) = rx.try_recv() else {
    //         panic!("Didn't receive the block")
    //     };
    //
    //     assert_eq!(block, announced_block);
    // }

    #[tokio::test]
    async fn will_not_propagate_a_stale_block() {
        let last_block_submission = given_an_pending_submission(2);

        {
            let current_block = given_a_block(1);

            let should_propagate = BlockWatcher::should_propagate_update(
                1,
                &current_block,
                Some(&last_block_submission),
            );

            assert!(!should_propagate);
        }
        {
            let current_block = given_a_block(2);

            let should_propagate = BlockWatcher::should_propagate_update(
                1,
                &current_block,
                Some(&last_block_submission),
            );

            assert!(!should_propagate);
        }
    }

    #[tokio::test]
    async fn will_propagate_even_if_last_tx_is_pending() {
        let current_block = given_a_block(2);
        let last_block_submission = given_an_pending_submission(1);

        let should_propagate =
            BlockWatcher::should_propagate_update(1, &current_block, Some(&last_block_submission));

        assert!(should_propagate);
    }

    #[tokio::test]
    async fn respects_epoch_when_posting_block_updates() {
        let commit_epoch = 3;

        let check_should_submit = |block_height, should_submit| {
            let current_block = given_a_block(block_height);
            let actual = BlockWatcher::should_propagate_update(commit_epoch, &current_block, None);

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

        let block_fetcher = given_fetcher_that_returns(vec![given_a_block(5)]);

        let storage = SqliteDb::temporary().await.unwrap();
        storage
            .insert(given_an_pending_submission(4))
            .await
            .unwrap();

        let mut block_watcher = BlockWatcher::new(2, tx, block_fetcher, storage);

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

    fn given_fetcher_that_returns(blocks: Vec<FuelBlock>) -> MockBlockFetcher {
        let blocks = Arc::new(std::sync::Mutex::new(blocks));
        let mut fetcher = MockBlockFetcher::new();
        fetcher
            .expect_latest_block()
            .returning(move || Ok(blocks.lock().unwrap().pop().unwrap()));
        fetcher
    }

    fn given_an_pending_submission(block_height: u32) -> BlockSubmission {
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
