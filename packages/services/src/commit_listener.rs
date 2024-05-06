use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use metrics::{Collector, IntGauge, Opts, RegistersMetrics};
use ports::{
    eth_rpc::EthereumAdapter,
    storage::Storage,
    types::{EthHeight, FuelBlockCommittedOnEth},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::Runner;

pub struct CommitListener<E, Db> {
    ethereum_rpc: E,
    storage: Db,
    metrics: Metrics,
    cancel_token: CancellationToken,
}

impl<E, Db> CommitListener<E, Db> {
    pub fn new(ethereum_rpc: E, storage: Db, cancel_token: CancellationToken) -> Self {
        Self {
            ethereum_rpc,
            storage,
            metrics: Metrics::default(),
            cancel_token,
        }
    }
}

impl<E, Db> CommitListener<E, Db>
where
    E: EthereumAdapter,
    Db: Storage,
{
    async fn determine_starting_eth_block(&mut self) -> crate::Result<EthHeight> {
        Ok(self
            .storage
            .submission_w_latest_block()
            .await?
            .map_or(0u32.into(), |submission| submission.submittal_height))
    }

    async fn handle_block_committed(
        &self,
        committed_on_eth: FuelBlockCommittedOnEth,
    ) -> crate::Result<()> {
        info!("block comitted on eth {committed_on_eth:?}");

        let submission = self
            .storage
            .set_submission_completed(committed_on_eth.fuel_block_hash)
            .await?;

        self.metrics
            .latest_committed_block
            .set(i64::from(submission.block.height));

        Ok(())
    }

    fn log_if_error(result: crate::Result<()>) {
        if let Err(error) = result {
            error!("Received an error from block commit event stream: {error}");
        }
    }
}

#[async_trait]
impl<E, Db> Runner for CommitListener<E, Db>
where
    E: EthereumAdapter,
    Db: Storage,
{
    async fn run(&mut self) -> crate::Result<()> {
        let eth_block = self.determine_starting_eth_block().await?;

        self.ethereum_rpc
            .event_streamer(eth_block.into())
            .establish_stream()
            .await?
            .map_err(Into::into)
            .and_then(|event| self.handle_block_committed(event))
            .take_until(self.cancel_token.cancelled())
            .for_each(|response| async { Self::log_if_error(response) })
            .await;

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
    use metrics::{Metric, RegistersMetrics, Registry};
    use mockall::predicate;
    use ports::{
        eth_rpc::{MockEthereumAdapter, MockEventStreamer},
        storage::Storage,
        types::{BlockSubmission, EthHeight, FuelBlockCommittedOnEth, U256},
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
        let block_hash = submission.block.hash;

        let eth_rpc_mock =
            given_eth_rpc_that_will_stream(vec![block_hash], submission.submittal_height);

        let process = PostgresProcess::shared().await.unwrap();
        let db = db_with_submission(&process, submission).await;

        let mut commit_listener =
            CommitListener::new(eth_rpc_mock, db.clone(), CancellationToken::default());

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
        let block_hash = submission.block.hash;
        let fuel_block_height = submission.block.height;

        let eth_rpc_mock =
            given_eth_rpc_that_will_stream(vec![block_hash], submission.submittal_height);

        let process = PostgresProcess::shared().await.unwrap();
        let db = db_with_submission(&process, submission).await;

        let mut commit_listener =
            CommitListener::new(eth_rpc_mock, db, CancellationToken::default());

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

        let missing_hash = block_missing_from_db.block.hash;
        let incoming_hash = incoming_block.block.hash;

        let eth_rpc_mock = given_eth_rpc_that_will_stream(
            vec![missing_hash, incoming_hash],
            incoming_block.submittal_height,
        );

        let process = PostgresProcess::shared().await.unwrap();
        let db = db_with_submission(&process, incoming_block.clone()).await;

        let mut commit_listener =
            CommitListener::new(eth_rpc_mock, db.clone(), CancellationToken::default());

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

    fn given_eth_rpc_that_will_stream(
        events: Vec<[u8; 32]>,
        starting_from_height: EthHeight,
    ) -> MockEthereumAdapter {
        let mut eth_rpc = MockEthereumAdapter::new();

        let event_streamer = Box::new(given_event_streamer_w_events(events));
        eth_rpc
            .expect_event_streamer()
            .with(predicate::eq(u64::from(starting_from_height)))
            .return_once(move |_| event_streamer);

        eth_rpc
    }

    fn given_event_streamer_w_events(events: Vec<[u8; 32]>) -> MockEventStreamer {
        let mut streamer = MockEventStreamer::new();
        let events = events
            .into_iter()
            .map(|block_hash| FuelBlockCommittedOnEth {
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
