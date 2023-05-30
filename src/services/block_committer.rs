use async_trait::async_trait;
use fuels::types::block::Block as FuelBlock;
use tokio::sync::{mpsc::Receiver, Mutex};
use tracing::info;

use crate::{
    adapters::{
        ethereum_rpc::EthereumAdapter,
        runner::Runner,
        storage::{EthTxSubmission, Storage},
    },
    common::EthTxStatus,
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
            // listen for new blocks
            if let Some(block) = self.rx_block.lock().await.recv().await {
                let tx_hash = self.ethereum_rpc.submit(block.clone()).await?;

                self.storage
                    .insert(EthTxSubmission {
                        fuel_block_height: block.header.height,
                        status: EthTxStatus::Pending,
                        tx_hash,
                    })
                    .await?;

                info!("tx: {} pending", &tx_hash);
            }
        }
    }
}
