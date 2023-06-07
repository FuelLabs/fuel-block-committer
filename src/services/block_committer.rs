use async_trait::async_trait;
use fuels::types::block::Block as FuelBlock;
use tokio::sync::{mpsc::Receiver, Mutex};
use tracing::{error};

use crate::{
    adapters::{ethereum_adapter::EthereumAdapter, runner::Runner, storage::{Storage, BlockSubmission}},
    errors::Result,
};

#[allow(dead_code)]
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
        // listen for new blocks
        while let Some(block) = self.rx_block.lock().await.recv().await {
            let submitted_at_height = self.ethereum_rpc.get_latest_eth_block().await?;
            let fuel_block_height = block.header.height;

            let submission = BlockSubmission {
                fuel_block_height,
                submitted_at_height,
                fuel_block_hash: block.id,
                completed: false,
            };

            let maybe_error = match self.ethereum_rpc.submit(block).await {
                Ok(_) => self
                    .storage
                    .insert(submission)
                    .await
                    .err(),
                Err(e) => Some(e),
            };

            if let Some(error) = maybe_error {
                error!("{error}");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ethers::types::{H256, U64};
    use fuels::{tx::Bytes32, types::block::Header as FuelBlockHeader};
    use mockall::predicate;

    use super::*;
    use crate::adapters::{storage::sqlite_db::SqliteDb, ethereum_adapter::MockEthereumAdapter};

    #[tokio::test]
    async fn block_committer_will_submit_and_write_block() {
        // given
        let block_height = 5;
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let block = given_a_block(block_height);
        let storage = SqliteDb::temporary().await.unwrap();
        let eth_rpc_mock = given_eth_rpc_that_expects(block.clone());
        tx.try_send(block.clone()).unwrap();

        // when
        spawn_committer_and_run_until_timeout(rx, eth_rpc_mock, storage.clone()).await;

        //then
        let last_submission = storage.submission_w_latest_block().await.unwrap().unwrap();
        assert_eq!(block_height, last_submission.fuel_block_height);
    }

    fn given_a_block(block_height: u32) -> FuelBlock {
        let header = FuelBlockHeader {
            id: Bytes32::zeroed(),
            da_height: 0,
            transactions_count: 0,
            message_receipt_count: 0,
            transactions_root: Bytes32::zeroed(),
            message_receipt_root: Bytes32::zeroed(),
            height: block_height,
            prev_root: Bytes32::zeroed(),
            time: None,
            application_hash: Bytes32::zeroed(),
        };

        FuelBlock {
            id: Bytes32::default(),
            header,
            transactions: vec![],
        }
    }

    fn given_tx_hash() -> H256 {
        "0x049d33c83c7c4115521d47f5fd285ee9b1481fe4a172e4f208d685781bea1ecc"
            .parse()
            .unwrap()
    }

    fn given_eth_rpc_that_expects(block: FuelBlock) -> MockEthereumAdapter {
        let mut eth_rpc_mock = MockEthereumAdapter::new();
        eth_rpc_mock
            .expect_submit()
            .with(predicate::eq(block))
            .return_once(move |_| Ok(()));

        eth_rpc_mock
            .expect_get_latest_eth_block()
            .return_once(move || Ok(U64::default()));

        eth_rpc_mock
    }

    async fn spawn_committer_and_run_until_timeout(
        rx: Receiver<FuelBlock>,
        eth_rpc_mock: MockEthereumAdapter,
        storage: impl Storage + 'static,
    ) {
        let _ = tokio::time::timeout(Duration::from_millis(250), async move {
            let block_committer = BlockCommitter::new(rx, eth_rpc_mock, storage);
            block_committer
                .run()
                .await
                .expect("Errors are handled inside of run");
        })
        .await;
    }
}
