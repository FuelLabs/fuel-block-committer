use fuels::types::block::Block as FuelBlock;
use tokio::sync::mpsc::Sender;

use crate::{
    adapters::{block_fetcher::BlockFetcher, storage::Storage},
    common::EthTxStatus,
};

pub struct BlockWatcher {
    block_fetcher: Box<dyn BlockFetcher + Send + Sync>,
    tx_fuel_block: Sender<FuelBlock>,
    storage: Box<dyn Storage + Send + Sync>,
    commit_epoch: u32,
}

impl BlockWatcher {
    pub fn new(
        commit_epoch: u32,
        tx_fuel_block: Sender<FuelBlock>,
        block_fetcher: impl BlockFetcher + 'static + Send + Sync,
        storage: impl Storage + 'static + Send + Sync,
    ) -> Self {
        Self {
            commit_epoch,
            block_fetcher: Box::new(block_fetcher),
            tx_fuel_block,
            storage: Box::new(storage),
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let fuel_block = self.block_fetcher.latest_block().await?; // todo: handle error

        let latest_submission = self.storage.submission_w_latest_block().await?;
        if let Some(submission) = latest_submission {
            if submission.fuel_block_height >= fuel_block.header.height {
                return Ok(());
            }

            match submission.status {
                EthTxStatus::Pending => {}
                EthTxStatus::Commited => {
                    if (fuel_block.header.height - submission.fuel_block_height) % self.commit_epoch
                        != 0
                    {
                        return Ok(());
                    }
                }
                EthTxStatus::Aborted => {}
            }
        }

        // check is it time for new epoch

        // if yes send the block to the committer
        self.tx_fuel_block.send(fuel_block).await?; // todo: handle error

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, vec};

    use ethers::types::H256;
    use fuels::{tx::Bytes32, types::block::Header as FuelBlockHeader};

    use super::*;
    use crate::{
        adapters::{
            block_fetcher::MockBlockFetcher,
            storage::{EthTxSubmission, InMemoryStorage},
        },
        common::EthTxStatus,
    };
    // TODO: TESTS
    // * epoch, first after failure
    // * pending tx

    #[tokio::test]
    async fn will_propagate_a_received_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let block = given_a_block(0);

        let block_fetcher = given_fetcher_that_returns(vec![block.clone()]);

        let storage = InMemoryStorage::new();
        let block_watcher = BlockWatcher::new(1, tx, block_fetcher, storage);

        // when
        block_watcher.run().await.unwrap();

        //then
        let Ok(announced_block) = rx.try_recv() else {
            panic!("Didn't receive the block")
        };

        assert_eq!(block, announced_block);
    }

    #[tokio::test]
    async fn will_not_propagate_if_last_tx_is_pending() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let block = given_a_block(0);

        let storage = InMemoryStorage::new();
        storage.insert(given_pending_submission(1)).await.unwrap();

        let block_fetcher = given_fetcher_that_returns(vec![block.clone()]);

        let block_watcher = BlockWatcher::new(1, tx, block_fetcher, storage);

        // when
        block_watcher.run().await.unwrap();

        //then
        let err = rx.try_recv().expect_err("Should have failed");
        assert!(matches!(err, tokio::sync::mpsc::error::TryRecvError::Empty))
    }

    #[tokio::test]
    async fn respects_epoch_when_posting_block_updates() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let blocks = (1..=3)
            .map(|height| given_a_block(height))
            .collect::<Vec<_>>();

        let block_fetcher = given_fetcher_that_returns(blocks.clone());

        let epoch = 3;
        let storage = InMemoryStorage::new();
        storage
            .insert(EthTxSubmission {
                fuel_block_height: 0,
                status: EthTxStatus::Commited,
                tx_hash: Default::default(),
            })
            .await
            .unwrap();
        let block_watcher = BlockWatcher::new(epoch, tx, block_fetcher, storage);
        block_watcher.run().await.unwrap();
        block_watcher.run().await.unwrap();

        // when
        block_watcher.run().await.unwrap();

        //then
        assert_eq!(rx.try_recv().ok().unwrap(), *blocks.last().unwrap());

        let err = rx.try_recv().expect_err("Should have been empty");
        assert!(matches!(err, tokio::sync::mpsc::error::TryRecvError::Empty))
    }

    #[tokio::test]
    async fn will_post_the_next_block_after_failure() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let storage = InMemoryStorage::new();
        storage
            .insert(EthTxSubmission {
                fuel_block_height: 2,
                status: EthTxStatus::Aborted,
                tx_hash: H256::default(),
            })
            .await
            .unwrap();

        let new_block = given_a_block(3);
        let block_fetcher = given_fetcher_that_returns(vec![new_block.clone()]);

        let epoch = 2;
        let block_watcher = BlockWatcher::new(epoch, tx, block_fetcher, storage);

        // when
        block_watcher.run().await.unwrap();

        //then
        assert_eq!(rx.try_recv().ok().unwrap(), new_block);

        let err = rx.try_recv().expect_err("Should have been empty");
        assert!(matches!(err, tokio::sync::mpsc::error::TryRecvError::Empty))
    }

    fn given_fetcher_that_returns(blocks: Vec<FuelBlock>) -> MockBlockFetcher {
        let blocks = Arc::new(std::sync::Mutex::new(blocks));
        let mut fetcher = MockBlockFetcher::new();
        fetcher
            .expect_latest_block()
            .returning(move || Ok(blocks.lock().unwrap().pop().unwrap()));
        fetcher
    }

    fn given_pending_submission(block_height: u32) -> EthTxSubmission {
        EthTxSubmission {
            fuel_block_height: block_height,
            status: EthTxStatus::Pending,
            tx_hash: H256::default(),
        }
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
}
