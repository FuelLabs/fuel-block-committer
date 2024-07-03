use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use ports::{
    storage::Storage,
    types::{FuelBlockCommittedOnL1, L1Height},
};
use tracing::{error, info};

use super::Runner;

pub struct StateListener<L1, Db> {
    l1_adapter: L1,
    storage: Db,
    num_block_to_finalize: u64,
    metrics: Metrics,
}

impl<L1, Db> StateListener<L1, Db> {
    pub fn new(l1_adapter: L1, storage: Db, num_block_to_finalize: u64) -> Self {
        Self {
            l1_adapter,
            storage,
            num_block_to_finalize,
            metrics: Metrics::default(),
        }
    }
}

impl<L1, Db> StateListener<L1, Db>
where
    L1: ports::l1::Api,
    Db: Storage,
{
    async fn check_pending_txs(&mut self, pending_txs: Vec<[u8; 32]>) -> crate::Result<()> {
        let current_block_number: u64 = self.l1_adapter.get_block_number().await?.into();

        for tx in pending_txs {
            let Some(tx_receipt) = self.l1_adapter.get_transaction_receipt(tx).await? else {
                continue; // not committed
            };

            let tx_block_number: u64 = tx_receipt
                .block_number
                .expect("block number must be there as the block is committed")
                .try_into()
                .map_err(|_| {
                    crate::Error::Other("could not convert `block_number` to `u64`".to_string())
                })?;

            if current_block_number.saturating_sub(tx_block_number) < self.num_block_to_finalize {
                continue; // not finalized
            }

            //self.finalize_fragments(tx).await?;
        }

        Ok(())
    }

    async fn finalize_fragments(&mut self, tx: [u8; 32]) -> crate::Result<()> {
        // find framents with tx_hash set to complteand save all fuel_block_hashes

        // find all fragments with `fuel_block_hash` if all complted find submission and set to
        // completed

        Ok(())
    }
}

#[async_trait]
impl<L1, Db> Runner for StateListener<L1, Db>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage,
{
    async fn run(&mut self) -> crate::Result<()> {
        // TODO: @hal3e add run logic
        //
        // 1. Fetch pending txs from storage
        // 2. Check if empty -> early return
        // 3. Get eth block height
        // 4. If block committed compare block height with current block height
        // 5. If block finalized:
        //  5.1 Remove tx from pending tx
        //  5.2 Find all fragments for this tx_hash set to completed and save fuel_block_hash
        // 6. Find all fragments with fuel_block_hash and if all are completed find
        //    submission with fuel_block_hash and set to completed

        let pending_txs = self.storage.get_pending_txs().await?;

        if pending_txs.is_empty() {
            return Ok(());
        }

        self.check_pending_txs(pending_txs).await?;

        Ok(())
    }
}

// TODO: @hal3e what kind of metrics can we use here
#[derive(Clone)]
struct Metrics {
    latest_committed_block: IntGauge,
}

impl<L1, Db> RegistersMetrics for StateListener<L1, Db> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![Box::new(self.metrics.latest_committed_block.clone())]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let latest_committed_block = IntGauge::with_opts(Opts::new(
            "latest_committed_block",
            "The height of the latest fuel block committed on Ethereum.",
        ))
        .expect("latest_committed_block metric to be correctly configured");

        Self {
            latest_committed_block,
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::stream;
    use metrics::{
        prometheus::{proto::Metric, Registry},
        RegistersMetrics,
    };
    use mockall::predicate;
    use ports::{
        l1::{MockContract, MockEventStreamer},
        storage::Storage,
        types::{BlockSubmission, FuelBlockCommittedOnL1, L1Height, U256},
    };
    use rand::Rng;
    use storage::{Postgres, PostgresProcess};
    use tokio_util::sync::CancellationToken;

    use crate::{CommitListener, Runner};

    #[tokio::test]
    async fn listener_will_update_storage_if_event_is_emitted() {
        use ports::storage::Storage;
        // given
        let mut rng = rand::thread_rng();
        let submission = BlockSubmission {
            completed: false,
            ..rng.gen()
        };
        let block_hash = submission.block_hash;

        let contract = given_contract_with_events(vec![block_hash], submission.submittal_height);

        let process = PostgresProcess::shared().await.unwrap();
        let db = db_with_submission(&process, submission).await;

        let mut commit_listener =
            CommitListener::new(contract, db.clone(), CancellationToken::default());

        // when
        commit_listener.run().await.unwrap();

        //then
        let res = db.submission_w_latest_block().await.unwrap().unwrap();

        assert!(res.completed);
    }

    #[tokio::test]
    async fn listener_will_update_metrics_if_event_is_emitted() {
        // given
        let mut rng = rand::thread_rng();
        let submission = BlockSubmission {
            completed: false,
            ..rng.gen()
        };
        let block_hash = submission.block_hash;
        let fuel_block_height = submission.block_height;

        let contract = given_contract_with_events(vec![block_hash], submission.submittal_height);

        let process = PostgresProcess::shared().await.unwrap();
        let db = db_with_submission(&process, submission).await;

        let mut commit_listener = CommitListener::new(contract, db, CancellationToken::default());

        let registry = Registry::new();
        commit_listener.register_metrics(&registry);

        // when
        commit_listener.run().await.unwrap();

        //then
        let metrics = registry.gather();
        let latest_committed_block_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "latest_committed_block")
            .and_then(|metric| metric.get_metric().first())
            .map(Metric::get_gauge)
            .unwrap();

        assert_eq!(
            latest_committed_block_metric.get_value(),
            f64::from(fuel_block_height)
        );
    }

    #[tokio::test]
    async fn error_while_handling_event_will_not_close_stream() {
        // given
        let mut rng = rand::thread_rng();
        let block_missing_from_db: BlockSubmission = rng.gen();
        let incoming_block: BlockSubmission = rng.gen();

        let missing_hash = block_missing_from_db.block_hash;
        let incoming_hash = incoming_block.block_hash;

        let contract = given_contract_with_events(
            vec![missing_hash, incoming_hash],
            incoming_block.submittal_height,
        );

        let process = PostgresProcess::shared().await.unwrap();
        let db = db_with_submission(&process, incoming_block.clone()).await;

        let mut commit_listener =
            CommitListener::new(contract, db.clone(), CancellationToken::default());

        // when
        commit_listener.run().await.unwrap();

        //then
        let latest_submission = db.submission_w_latest_block().await.unwrap().unwrap();
        assert_eq!(
            BlockSubmission {
                completed: true,
                ..incoming_block.clone()
            },
            latest_submission
        );
    }

    async fn db_with_submission(
        process: &PostgresProcess,
        submission: BlockSubmission,
    ) -> Postgres {
        let db = process.create_random_db().await.unwrap();

        db.insert(submission).await.unwrap();

        db
    }

    fn given_contract_with_events(
        events: Vec<[u8; 32]>,
        starting_from_height: L1Height,
    ) -> MockContract {
        let mut contract = MockContract::new();

        let event_streamer = Box::new(given_event_streamer_w_events(events));
        contract
            .expect_event_streamer()
            .with(predicate::eq(starting_from_height))
            .return_once(move |_| event_streamer);

        contract
    }

    fn given_event_streamer_w_events(events: Vec<[u8; 32]>) -> MockEventStreamer {
        let mut streamer = MockEventStreamer::new();
        let events = events
            .into_iter()
            .map(|block_hash| FuelBlockCommittedOnL1 {
                fuel_block_hash: block_hash,
                commit_height: U256::default(),
            })
            .map(Ok)
            .collect::<Vec<_>>();

        streamer
            .expect_establish_stream()
            .return_once(move || Ok(Box::pin(stream::iter(events))));

        streamer
    }
}
