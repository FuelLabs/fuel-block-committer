use async_trait::async_trait;
use tracing::{error, info, log::warn};

use crate::{
    adapters::{ethereum_rpc::EthereumAdapter, runner::Runner, storage::Storage},
    common::EthTxStatus,
    errors::Result,
};

pub struct CommitListener {
    ethereum_rpc: Box<dyn EthereumAdapter>,
    storage: Box<dyn Storage + Send + Sync>,
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
        if let Some(submission) = self.storage.submission_w_latest_block().await? {
            if submission.status == EthTxStatus::Pending {
                let new_status = self.ethereum_rpc.poll_tx_status(submission.tx_hash).await?;

                match new_status {
                    EthTxStatus::Pending => warn!("tx: {} not commited", submission.tx_hash),
                    EthTxStatus::Committed => {
                        info!("tx: {} commited", submission.tx_hash);
                    }
                    EthTxStatus::Aborted => error!("tx: {} aborted", submission.tx_hash),
                }

                if new_status != EthTxStatus::Pending {
                    self.storage
                        .set_submission_status(submission.fuel_block_height, new_status)
                        .await?;
                }
            } else {
                warn!("no pending tx submission found in storage");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        adapters::{
            ethereum_rpc::MockEthereumAdapter,
            storage::{EthTxSubmission, InMemoryStorage},
        },
        common::EthTxStatus,
    };
    use ethers::types::H256;
    use mockall::predicate;

    #[tokio::test]
    async fn listener_will_not_write_when_new_satus_pending() {
        // given
        let (tx_hash, storage) = given_tx_hash_and_storage().await;
        let eth_rpc_mock = given_eth_rpc_that_returns(tx_hash, EthTxStatus::Pending);
        let commit_listener = CommitListener::new(eth_rpc_mock, storage.clone());

        // when
        commit_listener.run().await.unwrap();

        //then
        let res = storage.submission_w_latest_block().await.unwrap().unwrap();

        assert_eq!(res.status, EthTxStatus::Pending);
    }

    #[tokio::test]
    async fn listener_will_write_when_new_satus_commited() {
        // given
        let (tx_hash, storage) = given_tx_hash_and_storage().await;
        let eth_rpc_mock = given_eth_rpc_that_returns(tx_hash, EthTxStatus::Committed);
        let commit_listener = CommitListener::new(eth_rpc_mock, storage.clone());

        // when
        commit_listener.run().await.unwrap();

        //then
        let res = storage.submission_w_latest_block().await.unwrap().unwrap();

        assert_eq!(res.status, EthTxStatus::Committed);
    }

    #[tokio::test]
    async fn listener_will_write_when_new_satus_aborted() {
        // given
        let (tx_hash, storage) = given_tx_hash_and_storage().await;
        let eth_rpc_mock = given_eth_rpc_that_returns(tx_hash, EthTxStatus::Aborted);
        let commit_listener = CommitListener::new(eth_rpc_mock, storage.clone());

        // when
        commit_listener.run().await.unwrap();

        //then
        let res = storage.submission_w_latest_block().await.unwrap().unwrap();

        assert_eq!(res.status, EthTxStatus::Aborted);
    }

    fn given_eth_rpc_that_returns(tx_hash: H256, status: EthTxStatus) -> MockEthereumAdapter {
        let mut eth_rpc_mock = MockEthereumAdapter::new();
        eth_rpc_mock
            .expect_poll_tx_status()
            .with(predicate::eq(tx_hash))
            .return_once(|_| Ok(status));
        eth_rpc_mock
    }

    async fn given_tx_hash_and_storage() -> (H256, InMemoryStorage) {
        let tx_hash: H256 = "0x049d33c83c7c4115521d47f5fd285ee9b1481fe4a172e4f208d685781bea1ecc"
            .parse()
            .unwrap();

        let storage = InMemoryStorage::new();
        storage
            .insert(EthTxSubmission {
                fuel_block_height: 3,
                status: EthTxStatus::Pending,
                tx_hash,
            })
            .await
            .unwrap();

        (tx_hash, storage)
    }
}
