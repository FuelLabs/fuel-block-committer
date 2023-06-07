use async_trait::async_trait;

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
        self.ethereum_rpc.watch_events(eth_block).await
    }
}

#[cfg(test)]
mod tests {
    use ethers::types::H256;
    use fuels::tx::Bytes32;
    use mockall::predicate;

    use super::*;
    use crate::{
        adapters::{
            ethereum_adapter::MockEthereumAdapter,
            storage::{sqlite_db::SqliteDb, BlockSubmission},
        },
        common::EthTxStatus,
    };

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
