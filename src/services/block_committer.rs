use actix_web::dev::Url;
use fuels::types::block::Block as FuelBlock;
use tokio::sync::mpsc::Receiver;

pub struct BlockCommitter {
    rx_block: Receiver<FuelBlock>,
    etherem_rpc: Url,
}

impl BlockCommitter {
    pub fn new(rx_block: Receiver<FuelBlock>, etherem_rpc: Url) -> Self {
        Self {
            rx_block,
            etherem_rpc,
        }
    }

    // todo: this should probably run as a stream
    pub async fn run(&mut self) -> anyhow::Result<()> {
        // listen to the newly received block
        if let Some(_block) = self.rx_block.recv().await {
            // enhancment: consume all the blocks from the channel to get the latest one

            // commit the block to ethereum
        }

        Ok(())
    }
}
