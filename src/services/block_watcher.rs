use std::{sync::Arc, time::Duration};

use fuels::types::block::Block as FuelBlock;
use tokio::sync::{mpsc::Sender, Mutex};

use crate::adapters::{block_fetcher::BlockFetcher, storage::Storage};

pub struct BlockWatcher {
    block_fetcher: Box<dyn BlockFetcher + Send + Sync>,
    tx_fuel_block: Sender<FuelBlock>,
    storage: Box<dyn Storage + Send + Sync>,
}

impl BlockWatcher {
    pub fn new(
        tx_fuel_block: Sender<FuelBlock>,
        block_fetcher: impl BlockFetcher + 'static + Send + Sync,
        storage: impl Storage + 'static + Send + Sync,
    ) -> Self {
        Self {
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
        }
        // check is it time for new epoch

        // if yes send the block to the committer
        self.tx_fuel_block.send(fuel_block).await?; // todo: handle error

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ethers::types::H256;
    use fuels::{tx::Bytes32, types::block::Header as FuelBlockHeader};

    use super::*;
    use crate::{
        adapters::{
            block_fetcher::{self, MockBlockFetcher},
            storage::{EthTxSubmission, InMemoryStorage},
        },
        common::EthTxStatus,
    };
    // TODO: TESTS
    // * epoch, every nth block
    // * pending tx

    #[tokio::test]
    async fn will_propagate_a_received_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let block = given_a_block();

        let block_fetcher = given_fetcher_that_returns(block.clone());

        let storage = InMemoryStorage::new();
        let block_watcher = BlockWatcher::new(tx, block_fetcher, storage);

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

        let block = given_a_block();

        let storage = InMemoryStorage::new();
        storage.insert(given_pending_submission(1)).await.unwrap();

        let block_fetcher = given_fetcher_that_returns(block.clone());

        let block_watcher = BlockWatcher::new(tx, block_fetcher, storage);

        // when
        block_watcher.run().await.unwrap();

        //then
        let err = rx.try_recv().expect_err("Should have failed");
        assert!(matches!(err, tokio::sync::mpsc::error::TryRecvError::Empty))
    }

    fn given_fetcher_that_returns(block: FuelBlock) -> MockBlockFetcher {
        let mut block_fetcher = MockBlockFetcher::new();
        block_fetcher
            .expect_latest_block()
            .returning(move || Ok(block.clone()));
        block_fetcher
    }

    fn given_pending_submission(block_height: u32) -> EthTxSubmission {
        EthTxSubmission {
            fuel_block_height: block_height,
            status: EthTxStatus::Pending,
            tx_hash: H256::default(),
        }
    }

    fn given_a_block() -> FuelBlock {
        let header = FuelBlockHeader {
            id: Bytes32::zeroed(),
            da_height: 0,
            transactions_count: 0,
            message_receipt_count: 0,
            transactions_root: Bytes32::zeroed(),
            message_receipt_root: Bytes32::zeroed(),
            height: 0,
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
