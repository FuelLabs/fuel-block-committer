use async_trait::async_trait;
use fuels::types::block::Block as FuelBlock;
use tokio::sync::{mpsc::Receiver, Mutex};
use tracing::log::warn;

use crate::{
    adapters::{runner::Runner, storage::Storage, tx_submitter::TxSubmitter},
    errors::Result,
};

pub struct BlockCommitter {
    rx_block: Mutex<Receiver<FuelBlock>>,
    tx_submitter: Box<dyn TxSubmitter>,
    storage: Box<dyn Storage>,
}

impl BlockCommitter {
    pub fn new(
        rx_block: Receiver<FuelBlock>,
        tx_submitter: impl TxSubmitter + 'static,
        storage: impl Storage + 'static,
    ) -> Self {
        Self {
            rx_block: Mutex::new(rx_block),
            tx_submitter: Box::new(tx_submitter),
            storage: Box::new(storage),
        }
    }
}

#[async_trait]
impl Runner for BlockCommitter {
    async fn run(&self) -> Result<()> {
        loop {
            // listen to the newly received block
            if let Some(block) = self.rx_block.lock().await.recv().await {
                // enhancment: consume all the blocks from the channel to get the latest one
                let tx_hash = self.tx_submitter.submit(block.clone()).await?;
                self.storage
                    .insert(crate::adapters::storage::EthTxSubmission {
                        fuel_block_height: block.header.height,
                        status: crate::common::EthTxStatus::Pending,
                        tx_hash,
                    })
                    .await?;
                warn!("{:?}", self.storage);
            }
        }
    }
}
