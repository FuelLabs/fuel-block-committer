use fuels::types::block::Block as FuelBlock;
use tokio::sync::mpsc::Sender;

use crate::{
    adapters::{
        block_fetcher::BlockFetcher,
        storage::{EthTxSubmission, Storage},
    },
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
        let current_block = self.block_fetcher.latest_block().await?; // todo: handle error

        let latest_block_submission = self.storage.submission_w_latest_block().await?;

        if Self::should_propagate_update(
            self.commit_epoch,
            &current_block,
            latest_block_submission.as_ref(),
        ) {
            self.tx_fuel_block.send(current_block).await?; // todo: handle error
        }

        Ok(())
    }

    fn should_propagate_update(
        commit_epoch: u32,
        current_block: &FuelBlock,
        last_block_submission: Option<&EthTxSubmission>,
    ) -> bool {
        let Some(submission) = last_block_submission else {
            return true;
        };

        if submission.fuel_block_height >= current_block.header.height {
            return true;
        }

        let height_diff = current_block.header.height - submission.fuel_block_height;
        match submission.status {
            EthTxStatus::Pending => false,
            EthTxStatus::Commited if height_diff % commit_epoch != 0 => false,
            _ => true,
        }
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

    #[tokio::test]
    async fn will_propagate_a_received_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let block = given_a_block(5);

        let block_fetcher = given_fetcher_that_returns(vec![block.clone()]);

        let storage = InMemoryStorage::new();
        storage
            .insert(EthTxSubmission {
                fuel_block_height: 3,
                status: EthTxStatus::Commited,
                tx_hash: H256::default(),
            })
            .await
            .unwrap();
        let block_watcher = BlockWatcher::new(2, tx, block_fetcher, storage);

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
        let current_block = given_a_block(2);
        let last_block_submission = given_pending_submission(1);

        let should_propagate =
            BlockWatcher::should_propagate_update(1, &current_block, Some(&last_block_submission));

        assert!(!should_propagate);
    }

    #[tokio::test]
    async fn respects_epoch_when_posting_block_updates() {
        let commit_epoch = 3;

        let last_block_submission = EthTxSubmission {
            fuel_block_height: 1,
            status: EthTxStatus::Commited,
            tx_hash: Default::default(),
        };

        let check_should_submit = |block_height, should_submit| {
            let current_block = given_a_block(block_height);
            let actual = BlockWatcher::should_propagate_update(
                commit_epoch,
                &current_block,
                Some(&last_block_submission),
            );

            assert_eq!(actual, should_submit);
        };

        check_should_submit(2, false);
        check_should_submit(3, false);
        check_should_submit(4, true);
    }

    #[tokio::test]
    async fn will_post_the_next_block_after_failure() {
        // given
        let last_block_submission = EthTxSubmission {
            fuel_block_height: 2,
            status: EthTxStatus::Aborted,
            tx_hash: H256::default(),
        };
        let current_block = given_a_block(4);

        // when
        let should_propagate =
            BlockWatcher::should_propagate_update(7, &current_block, Some(&last_block_submission));

        // then
        assert!(should_propagate);
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
