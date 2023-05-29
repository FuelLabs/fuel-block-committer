use fuels::types::block::Block as FuelBlock;
use tokio::sync::mpsc::Receiver;

use crate::{
    adapters::{storage::Storage, tx_submitter::TxSubmitter},
    errors::Result,
};

pub struct BlockCommitter {
    rx_block: Receiver<FuelBlock>,
    tx_submitter: Box<dyn TxSubmitter + 'static + Sync + Send>,
    storage: Box<dyn Storage + Send + Sync>,
}

impl BlockCommitter {
    pub fn new(
        rx_block: Receiver<FuelBlock>,
        tx_submitter: impl TxSubmitter + 'static + Sync + Send,
        storage: impl Storage + 'static + Send + Sync,
    ) -> Self {
        Self {
            rx_block,
            tx_submitter: Box::new(tx_submitter),
            storage: Box::new(storage),
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            // listen to the newly received block
            if let Some(block) = self.rx_block.recv().await {
                // enhancment: consume all the blocks from the channel to get the latest one
                self.tx_submitter.submit(block.clone()).await?;
                self.storage
                    .update_submission_status(
                        block.header.height,
                        crate::common::EthTxStatus::Pending,
                    )
                    .await;
            }
        }
    }
}
