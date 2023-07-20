use async_trait::async_trait;
use tokio::sync::mpsc::Receiver;
use tracing::{error, info};

use crate::{
    adapters::{
        ethereum_adapter::EthereumAdapter,
        fuel_adapter::FuelBlock,
        runner::Runner,
        storage::{BlockSubmission, Storage},
    },
    errors::Result,
};

pub struct BlockCommitter {
    rx_block: Receiver<FuelBlock>,
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
            rx_block,
            ethereum_rpc: Box::new(ethereum_rpc),
            storage: Box::new(storage),
        }
    }

    async fn next_fuel_block_for_committal(&mut self) -> Option<FuelBlock> {
        self.rx_block.recv().await
    }

    async fn submit_block(&self, fuel_block: FuelBlock) -> Result<()> {
        let submitted_at_height = self.ethereum_rpc.get_block_number().await?;

        let submission = BlockSubmission {
            block: fuel_block,
            submittal_height: submitted_at_height,
            completed: false,
        };

        self.storage.insert(submission).await?;

        // if we have a network failure the DB entry will be left at completed:false.
        self.ethereum_rpc.submit(fuel_block).await?;

        Ok(())
    }
}

#[async_trait]
impl Runner for BlockCommitter {
    async fn run(&mut self) -> Result<()> {
        while let Some(fuel_block) = self.next_fuel_block_for_committal().await {
            if let Err(error) = self.submit_block(fuel_block).await {
                error!("{error}");
            } else {
                info!("submitted {fuel_block:?}!");
            }
        }

        info!("Block Committer stopped");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mockall::predicate;

    use super::*;
    use crate::adapters::{ethereum_adapter::MockEthereumAdapter, storage::sqlite_db::SqliteDb};

    #[tokio::test]
    async fn block_committer_will_submit_and_write_block() {
        // given
        let block_height = 5;
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let block = given_a_block(block_height);
        let storage = SqliteDb::temporary().await.unwrap();
        let eth_rpc_mock = given_eth_rpc_that_expects(block);
        tx.try_send(block).unwrap();

        // when
        spawn_committer_and_run_until_timeout(rx, eth_rpc_mock, storage.clone()).await;

        //then
        let last_submission = storage.submission_w_latest_block().await.unwrap().unwrap();
        assert_eq!(block_height, last_submission.block.height);
    }

    fn given_a_block(block_height: u32) -> FuelBlock {
        FuelBlock {
            hash: Default::default(),
            height: block_height,
        }
    }

    fn given_eth_rpc_that_expects(block: FuelBlock) -> MockEthereumAdapter {
        let mut eth_rpc_mock = MockEthereumAdapter::new();
        eth_rpc_mock
            .expect_submit()
            .with(predicate::eq(block))
            .return_once(move |_| Ok(()));

        eth_rpc_mock
            .expect_get_block_number()
            .return_once(move || Ok(0));

        eth_rpc_mock
    }

    async fn spawn_committer_and_run_until_timeout(
        rx: Receiver<FuelBlock>,
        eth_rpc_mock: MockEthereumAdapter,
        storage: impl Storage + 'static,
    ) {
        let _ = tokio::time::timeout(Duration::from_millis(250), async move {
            let mut block_committer = BlockCommitter::new(rx, eth_rpc_mock, storage);
            block_committer
                .run()
                .await
                .expect("Errors are handled inside of run");
        })
        .await;
    }
}
