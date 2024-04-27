use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use prometheus::{IntGauge, Opts};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::{
    adapters::{
        ethereum_adapter::{EthHeight, EthereumAdapter, FuelBlockCommittedOnEth},
        runner::Runner,
        storage::Storage,
    },
    errors::Result,
    telemetry::RegistersMetrics,
};

pub struct CommitListener {
    ethereum_rpc: Box<dyn EthereumAdapter>,
    storage: Box<dyn Storage>,
    metrics: Metrics,
    cancel_token: CancellationToken,
}

impl CommitListener {
    pub fn new(
        ethereum_rpc: impl EthereumAdapter + 'static,
        storage: impl Storage + 'static,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            ethereum_rpc: Box::new(ethereum_rpc),
            storage: Box::new(storage),
            metrics: Metrics::default(),
            cancel_token,
        }
    }

    async fn determine_starting_eth_block(&self) -> Result<EthHeight> {
        Ok(self
            .storage
            .submission_w_latest_block()
            .await?
            .map_or(0u32.into(), |submission| submission.submittal_height))
    }

    async fn handle_block_committed(
        &self,
        committed_on_eth: FuelBlockCommittedOnEth,
    ) -> Result<()> {
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

    fn log_if_error(result: Result<()>) {
        if let Err(error) = result {
            error!("Received an error from block commit event stream: {error}");
        }
    }
}

#[async_trait]
impl Runner for CommitListener {
    async fn run(&mut self) -> Result<()> {
        let eth_block = self.determine_starting_eth_block().await?;

        self.ethereum_rpc
            .event_streamer(eth_block.into())
            .establish_stream()
            .await?
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

impl RegistersMetrics for CommitListener {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
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
    use ethers::types::U256;
    use futures::stream;
    use mockall::predicate;
    use prometheus::{proto::Metric, Registry};
    use tokio_util::sync::CancellationToken;

    use crate::{
        adapters::{
            ethereum_adapter::{
                EthHeight, FuelBlockCommittedOnEth, MockEthereumAdapter, MockEventStreamer,
            },
            fuel_adapter::FuelBlock,
            runner::Runner,
            storage::{postgresql::PostgresProcess, BlockSubmission, Storage},
        },
        errors::Result,
        services::CommitListener,
        telemetry::RegistersMetrics,
    };

    #[tokio::test]
    async fn listener_will_update_storage_if_event_is_emitted() {
        // given
        let submission = BlockSubmission {
            completed: false,
            ..BlockSubmission::random()
        };
        let block_hash = submission.block.hash;

        let eth_rpc_mock =
            given_eth_rpc_that_will_stream(vec![Ok(block_hash)], submission.submittal_height);

        let process = start_postgres_with_submission(submission).await;

        let mut commit_listener =
            CommitListener::new(eth_rpc_mock, process.db(), CancellationToken::default());

        // when
        commit_listener.run().await.unwrap();

        //then
        let res = process
            .db()
            .submission_w_latest_block()
            .await
            .unwrap()
            .unwrap();

        assert!(res.completed);
    }

    #[tokio::test]
    async fn listener_will_update_metrics_if_event_is_emitted() {
        // given
        let submission = BlockSubmission {
            completed: false,
            ..BlockSubmission::random()
        };
        let block_hash = submission.block.hash;
        let fuel_block_height = submission.block.height;

        let eth_rpc_mock =
            given_eth_rpc_that_will_stream(vec![Ok(block_hash)], submission.submittal_height);

        let process = start_postgres_with_submission(submission).await;

        let mut commit_listener =
            CommitListener::new(eth_rpc_mock, process.db(), CancellationToken::default());

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
        let old_block_missing_from_db = BlockSubmission {
            completed: false,
            block: FuelBlock {
                hash: Default::default(),
                height: 10,
            },
            ..BlockSubmission::random()
        };

        let new_block = BlockSubmission {
            completed: false,
            block: FuelBlock {
                hash: Default::default(),
                height: 11,
            },
            ..BlockSubmission::random()
        };

        let older_hash = old_block_missing_from_db.block.hash;
        let newer_hash = new_block.block.hash;

        let eth_rpc_mock = given_eth_rpc_that_will_stream(
            vec![Ok(older_hash), Ok(newer_hash)],
            new_block.submittal_height,
        );

        let process = start_postgres_with_submission(new_block.clone()).await;
        let mut commit_listener =
            CommitListener::new(eth_rpc_mock, process.db(), CancellationToken::default());

        // when
        commit_listener.run().await.unwrap();

        //then
        let latest_submission = process
            .db()
            .submission_w_latest_block()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            BlockSubmission {
                completed: true,
                ..new_block.clone()
            },
            latest_submission
        );
    }

    async fn start_postgres_with_submission(submission: BlockSubmission) -> PostgresProcess {
        let storage = PostgresProcess::start().await.unwrap();

        storage.db().insert(submission).await.unwrap();

        storage
    }

    fn given_eth_rpc_that_will_stream(
        events: Vec<Result<[u8; 32]>>,
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

    fn given_event_streamer_w_events(events: Vec<Result<[u8; 32]>>) -> MockEventStreamer {
        let mut streamer = MockEventStreamer::new();
        let events = events
            .into_iter()
            .map(|e| {
                e.map(|fuel_block_hash| FuelBlockCommittedOnEth {
                    fuel_block_hash,
                    commit_height: U256::default(),
                })
            })
            .collect::<Vec<_>>();

        streamer
            .expect_establish_stream()
            .return_once(move || Ok(Box::pin(stream::iter(events))));

        streamer
    }
}
