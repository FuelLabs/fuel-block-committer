use futures::{StreamExt, TryStreamExt};
use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use ports::{
    storage::Storage,
    types::{FuelBlockCommittedOnL1, L1Height, TransactionState},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::Runner;

pub struct CommitListener<L1, Db> {
    l1_adapter: L1,
    storage: Db,
    metrics: Metrics,
    cancel_token: CancellationToken,
}

impl<L1, Db> CommitListener<L1, Db> {
    pub fn new(l1_adapter: L1, storage: Db, cancel_token: CancellationToken) -> Self {
        Self {
            l1_adapter,
            storage,
            metrics: Metrics::default(),
            cancel_token,
        }
    }
}

impl<L1, Db> CommitListener<L1, Db>
where
    L1: ports::l1::Api,
    Db: Storage,
{
    async fn determine_starting_l1_height(&mut self) -> crate::Result<L1Height> {
        Ok(self
            .storage
            .submission_w_latest_block()
            .await?
            .map_or(0u32.into(), |submission| submission.submittal_height))
    }

    async fn handle_block_committed(
        &self,
        committed_on_l1: FuelBlockCommittedOnL1,
    ) -> crate::Result<()> {
        info!("received block commit event from l1 {committed_on_l1:?}");

        // let submission = self
        //     .storage
        //     .set_submission_completed(committed_on_l1.fuel_block_hash)
        //     .await?;

        // self.metrics
        //     .latest_committed_block
        //     .set(i64::from(submission.block_height));

        Ok(())
    }

    fn log_if_error(result: crate::Result<()>) {
        if let Err(error) = result {
            error!("Received an error from block commit event stream: {error}");
        }
    }
}

impl<C, Db> Runner for CommitListener<C, Db>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage,
{
    async fn run(&mut self) -> crate::Result<()> {
        let current_block_number: u64 = self.l1_adapter.get_block_number().await?.into();
        let transactions = self.storage.get_pending_contracts_txs().await?;

        for tx in transactions {
            let tx_hash = tx.hash;
            let Some(tx_response) = self.l1_adapter.get_transaction_response(tx_hash).await? else {
                continue; // not included
            };

            if !tx_response.succeeded() {
                self.storage
                    .update_submission_tx_state(tx_hash, TransactionState::Failed)
                    .await?;

                info!("failed contract commit tx {}", hex::encode(tx_hash));
                continue;
            }

            //TODO: self.num_blocks_to_finalize
            if !tx_response.confirmations(current_block_number) < 10 {
                continue; // not finalized
            }

            self.storage
                .update_block_submission_tx_state(tx_hash, TransactionState::Finalized)
                .await?;

            info!("finalized contract commit tx {}", hex::encode(tx_hash));

            let submission = self
                .storage
                .set_submission_completed(committed_on_l1.fuel_block_hash)
                .await?;

            self.metrics
                .latest_committed_block
                .set(i64::from(submission.block_height));
        }

        Ok(())
    }
}

#[derive(Clone)]
struct Metrics {
    latest_committed_block: IntGauge,
}

impl<E, Db> RegistersMetrics for CommitListener<E, Db> {
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
    use storage::{DbWithProcess, PostgresProcess};
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

        let db = db_with_submission(submission).await;

        let mut commit_listener =
            CommitListener::new(contract, db.clone(), CancellationToken::default());

        // when
        commit_listener.run().await.unwrap();

        // then
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

        let db = db_with_submission(submission).await;

        let mut commit_listener = CommitListener::new(contract, db, CancellationToken::default());

        let registry = Registry::new();
        commit_listener.register_metrics(&registry);

        // when
        commit_listener.run().await.unwrap();

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

        let db = db_with_submission(incoming_block.clone()).await;

        let mut commit_listener =
            CommitListener::new(contract, db.clone(), CancellationToken::default());

        // when
        commit_listener.run().await.unwrap();

        // then
        let latest_submission = db.submission_w_latest_block().await.unwrap().unwrap();
        assert_eq!(
            BlockSubmission {
                completed: true,
                ..incoming_block.clone()
            },
            latest_submission
        );
    }

    async fn db_with_submission(submission: BlockSubmission) -> DbWithProcess {
        let db = PostgresProcess::shared()
            .await
            .unwrap()
            .create_random_db()
            .await
            .unwrap();

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
