use async_trait::async_trait;
use ports::{
    storage::Storage,
    types::{BlockSubmission, FuelBlock},
};
use tokio::sync::mpsc::Receiver;
use tracing::{error, info};

use super::Runner;
use crate::Result;

pub struct BlockCommitter<C, Db> {
    rx_block: Receiver<FuelBlock>,
    ethereum_rpc: C,
    storage: Db,
}

impl<A, Db> BlockCommitter<A, Db> {
    pub fn new(rx_block: Receiver<FuelBlock>, ethereum_rpc: A, storage: Db) -> Self {
        Self {
            rx_block,
            ethereum_rpc,
            storage,
        }
    }

    async fn next_fuel_block_for_committal(&mut self) -> Option<FuelBlock> {
        self.rx_block.recv().await
    }
}

impl<A, Db> BlockCommitter<A, Db>
where
    A: ports::l1::Contract,
    Db: Storage,
{
    async fn submit_block(&self, fuel_block: FuelBlock) -> Result<()> {
        let submittal_height = self.ethereum_rpc.get_block_number().await?;

        let submission = BlockSubmission {
            block: fuel_block,
            submittal_height,
            completed: false,
        };

        self.storage.insert(submission).await?;

        // if we have a network failure the DB entry will be left at completed:false.
        self.ethereum_rpc.submit(fuel_block).await?;

        Ok(())
    }
}

#[async_trait]
impl<A, Db> Runner for BlockCommitter<A, Db>
where
    A: ports::l1::Contract,
    Db: Storage,
{
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
    use ports::l1::MockContract;
    use rand::Rng;
    use storage::PostgresProcess;

    use super::*;

    #[tokio::test]
    async fn block_committer_will_submit_and_write_block() {
        // given
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let block: FuelBlock = rand::thread_rng().gen();
        let expeted_height = block.height;
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();

        let eth_rpc_mock = given_eth_rpc_that_expects(block);
        tx.try_send(block).unwrap();

        // when
        spawn_committer_and_run_until_timeout(rx, eth_rpc_mock, db.clone()).await;

        // then
        let last_submission = db.submission_w_latest_block().await.unwrap().unwrap();
        assert_eq!(expeted_height, last_submission.block.height);
    }

    fn given_eth_rpc_that_expects(block: FuelBlock) -> MockContract {
        let mut eth_rpc_mock = MockContract::new();
        eth_rpc_mock
            .expect_submit()
            .with(predicate::eq(block))
            .return_once(move |_| Ok(()));

        eth_rpc_mock
            .expect_get_block_number()
            .return_once(move || Ok(0u32.into()));

        eth_rpc_mock
    }

    async fn spawn_committer_and_run_until_timeout<Db: Storage>(
        rx: Receiver<FuelBlock>,
        eth_rpc_mock: MockContract,
        storage: Db,
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
