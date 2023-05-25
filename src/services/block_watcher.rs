use std::{sync::Arc, time::Duration};

use fuels::types::block::Block as FuelBlock;
use tokio::sync::mpsc::Sender;

use crate::adapters::block_fetcher::BlockFetcher;

pub struct BlockWatcher {
    check_interval: Duration,
    block_fetcher: Box<dyn BlockFetcher + Send + Sync>,
    last_block_height: u64,
    tx_fuel_block: Sender<FuelBlock>,
}

impl BlockWatcher {
    pub fn new(
        check_interval: Duration,
        tx_fuel_block: Sender<FuelBlock>,
        block_fetcher: impl BlockFetcher + 'static + Send + Sync,
    ) -> Self {
        Self {
            check_interval,
            block_fetcher: Box::new(block_fetcher),
            tx_fuel_block,
            last_block_height: 0, // todo: block height should be provided as well
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        // get latest block on some interval + loop
        let fuel_block = self.block_fetcher.latest_block().await?; // todo: handle error

        // check is it time for new epoch

        // if yes send the block to the committer
        self.tx_fuel_block.send(fuel_block).await?; // todo: handle error

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::block_fetcher::MockBlockFetcher;
    use fuels::tx::Bytes32;
    use fuels::types::block::Header as FuelBlockHeader;

    #[tokio::test]
    async fn will_propagate_a_received_block() {
        // given
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        let block = given_a_block();

        let block_fetcher = {
            let mut block_fetcher = MockBlockFetcher::new();
            let block_to_ret = block.clone();
            block_fetcher
                .expect_latest_block()
                .returning(move || Ok(block_to_ret.clone()));
            block_fetcher
        };

        let block_watcher = BlockWatcher::new(Duration::from_millis(100), tx, block_fetcher);

        // when
        block_watcher.run().await.unwrap();

        //then
        let Ok(announced_block) = rx.try_recv() else {
            panic!("Didn't receive the block")
        };

        assert_eq!(block, announced_block);
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
