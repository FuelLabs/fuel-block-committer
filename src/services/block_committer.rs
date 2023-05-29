use fuels::types::block::Block as FuelBlock;
use tokio::sync::mpsc::Receiver;

use crate::{adapters::tx_submitter::TxSubmitter, errors::Result};

pub struct BlockCommitter {
    rx_block: Receiver<FuelBlock>,
    tx_submitter: Box<dyn TxSubmitter + 'static + Sync + Send>,
}

impl BlockCommitter {
    pub fn new(
        rx_block: Receiver<FuelBlock>,
        tx_submitter: impl TxSubmitter + 'static + Sync + Send,
    ) -> Self {
        Self {
            rx_block,
            tx_submitter: Box::new(tx_submitter),
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            // listen to the newly received block
            if let Some(block) = self.rx_block.recv().await {
                // enhancment: consume all the blocks from the channel to get the latest one
                self.tx_submitter.submit(block).await?;
            }
        }
    }
}
