use async_trait::async_trait;
use ethers::prelude::*;
use tracing::log::warn;
use url::Url;

use crate::{
    adapters::{runner::Runner, storage::Storage},
    common::EthTxStatus,
    errors::{Error, Result},
};

pub struct CommitListener {
    provider: Provider<Http>,
    storage: Box<dyn Storage + Send + Sync>,
}

impl CommitListener {
    pub fn new(ethereum_rpc: Url, storage: impl Storage + 'static + Send + Sync) -> Self {
        let provider = Provider::<Http>::try_from(ethereum_rpc.to_string()).unwrap();

        Self {
            provider,
            storage: Box::new(storage),
        }
    }

    fn handle_network_error(&self) {
        todo!()
    }

    async fn check_tx_finalized(&self, tx_hash: H256) -> Result<Option<TransactionReceipt>> {
        self.provider
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(|err| {
                self.handle_network_error();
                Error::NetworkError(err.to_string())
            })
    }
}

#[async_trait]
impl Runner for CommitListener {
    async fn run(&self) -> Result<()> {
        if let Some(submission) = self.storage.submission_w_latest_block().await? {
            if submission.status == EthTxStatus::Pending {
                // todo: what to do when tx reverts, is it possible?
                self.check_tx_finalized(submission.tx_hash).await?;

                self.storage
                    .set_submission_commited(submission.fuel_block_height)
                    .await?;

                warn!("{:?}", self.storage);
            }

            // add to metrics
        } else {
            warn!("no submission found in storage");
        }

        Ok(())
    }
}
