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
                        self.storage
                            .set_submission_status(submission.fuel_block_height, new_status)
                            .await?;
                        info!("tx: {} commited", submission.tx_hash);
                    }
                    EthTxStatus::Aborted => error!("tx: {} aborted", submission.tx_hash),
                }
            } else {
                warn!("no pending tx submission found in storage");
            }
        }

        Ok(())
    }
}
