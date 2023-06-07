use async_trait::async_trait;
use futures::StreamExt;
use tracing::info;

use crate::{
    adapters::{ethereum_adapter::EthereumAdapter, runner::Runner, storage::Storage},
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
        let eth_block = self.storage.latest_eth_block().await.unwrap_or(0);
        let commit_streamer = self.ethereum_rpc.commit_streamer(eth_block).unwrap();
        let mut stream = commit_streamer.stream().await;
        while let Some(Ok(a)) = stream.next().await {
            self.storage.set_submission_completed(a).await?;
            info!("block with hash: {:x} completed", a);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /*
    #[tokio::test]
    async fn listener_will_update_storage_if_event_is_emitted() {
        // given
        let tx_hash = given_tx_hash();
        let storage = given_storage(tx_hash).await;
        let eth_rpc_mock = given_eth_rpc_that_returns(tx_hash, EthTxStatus::Committed);
        let commit_listener = CommitListener::new(eth_rpc_mock, storage.clone());

        // when
        commit_listener.run().await.unwrap();

        //then
        let res = storage.submission_w_latest_block().await.unwrap().unwrap();

        assert!(res.completed);
    }

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

    async fn given_storage(tx_hash: H256) -> SqliteDb {
        let storage = SqliteDb::temporary().await.unwrap();
        storage
            .insert(BlockSubmission {
                fuel_block_height: 3,
                fuel_block_hash: Bytes32::default(),
                completed: false,
                submitted_at_height: Default::default(),
            })
            .await
            .unwrap();

        storage
    }
     */
}
