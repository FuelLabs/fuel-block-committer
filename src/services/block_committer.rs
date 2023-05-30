use async_trait::async_trait;
use fuels::types::block::Block as FuelBlock;
use tokio::sync::{mpsc::Receiver, Mutex};

use crate::{
    adapters::{ethereum_rpc::EthereumAdapter, runner::Runner, storage::Storage},
    errors::Result,
};

pub struct BlockCommitter {
    rx_block: Mutex<Receiver<FuelBlock>>,
    ethereum_rpc: Box<dyn EthereumAdapter>,
    storage: Box<dyn Storage>,
}

impl BlockCommitter {
    pub fn new(
        rx_block: Receiver<FuelBlock>,
        ethereum_rpc: impl EthereumAdapter + 'static,
        storage: impl Storage + 'static,
    ) -> Self {
        Self {
            rx_block: Mutex::new(rx_block),
            ethereum_rpc: Box::new(ethereum_rpc),
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
                let tx_hash = self.ethereum_rpc.submit(block.clone()).await?;
                self.storage
                    .insert(crate::adapters::storage::EthTxSubmission {
                        fuel_block_height: block.header.height,
                        status: crate::common::EthTxStatus::Pending,
                        tx_hash,
                    })
                    .await?;
            }
        }
    }
}
