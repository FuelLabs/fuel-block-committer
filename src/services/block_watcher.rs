use std::time::Duration;

use crate::{
    actors::messages::{BlockUpdate, CheckNewBlock},
    adapters::block_fetcher::BlockFetcher,
};

use fuels::types::block::Block as FuelBlock;
use tokio::sync::mpsc::Sender;

pub struct BlockWatcher {
    check_interval: Duration,
    block_fetcher: Box<dyn BlockFetcher>,
    last_block_height: u64,
    tx_fuel_block: Sender<FuelBlock>,
}

impl BlockWatcher {
    pub fn new(
        check_interval: Duration,
        tx_fuel_block: Sender<FuelBlock>,
        block_fetcher: impl BlockFetcher + 'static,
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
