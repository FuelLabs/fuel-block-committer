use async_trait::async_trait;
use futures::StreamExt;
use tracing::{error, info, warn};

use crate::{
    adapters::{
        ethereum_adapter::{BlockCommittedEventStreamer, EthereumAdapter},
        runner::Runner,
        storage::Storage,
    },
    errors::Result,
};

pub struct CommitListener {
    ethereum_rpc: Box<dyn EthereumAdapter>,
    storage: Box<dyn Storage>,
}

impl CommitListener {
    pub fn new(
        ethereum_rpc: impl EthereumAdapter + 'static,
        storage: impl Storage + 'static,
    ) -> Self {
        Self {
            ethereum_rpc: Box::new(ethereum_rpc),
            storage: Box::new(storage),
        }
    }
}

#[async_trait]
impl Runner for CommitListener {
    async fn run(&self) -> Result<()> {
        let eth_block = self
            .storage
            .submission_w_latest_block()
            .await?
            .map(|submission| submission.submitted_at_height.as_u64())
            .unwrap_or(0);

        let commit_streamer = self.ethereum_rpc.block_committed_event_streamer(eth_block);

        let mut stream = commit_streamer.establish_stream().await?;

        while let Some(event) = stream.next().await {
            match event {
                Ok(block_hash) => {
                    self.storage.set_submission_completed(block_hash).await?;
                    info!("block with hash: {:x} completed", block_hash);
                }
                Err(error) => {
                    error!("Received an error from block commit event stream: {error}");
                }
            }
        }

        warn!("Block commit event stream finished!");

        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use fuels::tx::Bytes32;
    use futures::stream;
    use mockall::predicate;
    use rand::Rng;

    use crate::{
        adapters::{
            ethereum_adapter::{MockBlockCommittedEventStreamer, MockEthereumAdapter},
            runner::Runner,
            storage::{sqlite_db::SqliteDb, BlockSubmission, Storage},
        },
        errors::Result,
        services::CommitListener,
    };

    #[tokio::test]
    async fn listener_will_update_storage_if_event_is_emitted() {
        // given
        let submission = BlockSubmission {
            completed: false,
            ..BlockSubmission::random()
        };
        let block_hash = submission.fuel_block_hash;

        let eth_rpc_mock = given_eth_rpc_that_will_stream(
            vec![Ok(block_hash)],
            submission.submitted_at_height.as_u64(),
        );

        let storage = given_storage_containing(submission).await;
        let commit_listener = CommitListener::new(eth_rpc_mock, storage.clone());

        // when
        commit_listener.run().await.unwrap();

        //then
        let res = storage.submission_w_latest_block().await.unwrap().unwrap();

        assert!(res.completed);
    }

    async fn given_storage_containing(submission: BlockSubmission) -> SqliteDb {
        let storage = SqliteDb::temporary().await.unwrap();
        storage.insert(submission).await.unwrap();

        storage
    }

    fn given_eth_rpc_that_will_stream(
        events: Vec<Result<Bytes32>>,
        starting_from_height: u64,
    ) -> MockEthereumAdapter {
        let mut eth_rpc = MockEthereumAdapter::new();

        let event_streamer = Box::new(given_event_streamer_w_events(events));
        eth_rpc
            .expect_block_committed_event_streamer()
            .with(predicate::eq(starting_from_height))
            .return_once(move |_| event_streamer);

        eth_rpc
    }

    fn given_random_block_hash() -> Bytes32 {
        let bytes = rand::thread_rng().gen::<[u8; 32]>();
        Bytes32::from(bytes)
    }

    fn given_event_streamer_w_events(
        events: Vec<Result<Bytes32>>,
    ) -> MockBlockCommittedEventStreamer {
        let mut streamer = MockBlockCommittedEventStreamer::new();

        streamer
            .expect_establish_stream()
            .return_once(move || Ok(Box::pin(stream::iter(events))));

        streamer
    }

    /*
    fn given_eth_rpc_that_returns(tx_hash: H256, status: EthTxStatus) -> MockEthereumAdapter {
        let mut eth_rpc_mock = MockEthereumAdapter::new();
        eth_rpc_mock
            .expect_()
            .with(predicate::eq(tx_hash))
            .return_once(|_| Ok(status));
        eth_rpc_mock
    }

    fn given_tx_hash() -> H256 {
        "0x049d33c83c7c4115521d47f5fd285ee9b1481fe4a172e4f208d685781bea1ecc"
            .parse()
            .unwrap()
    }
     */
}
