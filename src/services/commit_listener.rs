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
                // TODO: add metrics
                let new_status = self.ethereum_rpc.poll_tx_status(submission.tx_hash).await?;

                match new_status {
                    EthTxStatus::Pending => warn!("tx: {} not commited", submission.tx_hash),
                    EthTxStatus::Commited => {
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

    #[tokio::test]
    async fn will_write_new_eth_status() {
        // given
        let storage = InMemoryStorage::new();
        storage
            .insert(EthTxSubmission {
                fuel_block_height: 3,
                status: EthTxStatus::Pending,
                tx_hash: H256::default(),
            })
            .await
            .unwrap();
        let block_watcher = CommitListener::new(2, tx, block_fetcher, storage);

        // when
        block_watcher.run().await.unwrap();

        //then
        let Ok(announced_block) = rx.try_recv() else {
            panic!("Didn't receive the block")
        };

        assert_eq!(block, announced_block);
    }
}
