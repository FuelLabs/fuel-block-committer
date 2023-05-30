use async_trait::async_trait;
use ethers::types::H256;
use fuels::types::block::Block as FuelBlock;
use tokio::sync::{mpsc::Receiver, Mutex};
use tracing::{error, info};

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

    async fn write_storage(&self, tx_hash: H256, block_height: u32) -> Result<()> {
        self.storage
            .insert(EthTxSubmission {
                fuel_block_height: block_height,
                status: EthTxStatus::Pending,
                tx_hash,
            })
            .await?;

        info!("tx: {} pending", &tx_hash);
        Ok(())
    }
}

#[async_trait]
impl Runner for BlockCommitter {
    async fn run(&self) -> Result<()> {
        // listen for new blocks
        while let Some(block) = self.rx_block.lock().await.recv().await {
            let block_height = block.header.height;

            let maybe_error = match self.ethereum_rpc.submit(block.clone()).await {
                Ok(tx_hash) => self.write_storage(tx_hash, block_height).await.err(),
                Err(e) => Some(e),
            };

            if let Some(error) = maybe_error {
                error!("{error}");
            }
        }
        Ok(())
    }
}
