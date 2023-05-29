use ethers::prelude::*;
use url::Url;

use crate::{
    adapters::storage::Storage,
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

    pub async fn run(&self) -> Result<()> {
        let last_submission = self.storage.submission_w_latest_block().await?;

        if let Some(submission) = last_submission {
            // todo: what to do when tx reverts, is it possible?
            self.check_tx_finalized(submission.tx_hash).await?;

            self.storage
                .update_submission_status(submission.fuel_block_height, EthTxStatus::Commited)
                .await?;

            // add to metrics
        } else {
            // log no submission found
            todo!()
        }

        Ok(())
    }

    async fn check_tx_finalized(&self, tx_hash: H256) -> Result<Option<TransactionReceipt>> {
        Ok(self
            .provider
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(|err| {
                self.handle_network_error();
                Error::NetworkError(err.to_string())
            })?)
    }
}
