use actix_web::dev::Url;
use fuels::types::block::Block as FuelBlock;
use tokio::sync::mpsc::Receiver;

use crate::errors::Result;

#[allow(dead_code)]
pub struct BlockCommitter {
    rx_block: Receiver<FuelBlock>,
    etherem_rpc: Url,
}

impl BlockCommitter {
    #[allow(dead_code)]
    pub fn new(rx_block: Receiver<FuelBlock>, etherem_rpc: Url) -> Self {
        Self {
            rx_block,
            etherem_rpc,
        }
    }

    // todo: this should probably run as a stream
    #[allow(dead_code)]
    pub async fn run(&mut self) -> Result<()> {
        // listen to the newly received block
        if let Some(_block) = self.rx_block.recv().await {
            // enhancment: consume all the blocks from the channel to get the latest one

            // commit the block to ethereum
        }

        Ok(())
    }
}
