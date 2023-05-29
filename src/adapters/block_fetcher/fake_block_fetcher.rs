use crate::adapters::block_fetcher::BlockFetcher;
use crate::errors::Result;

use async_trait::async_trait;
use fuels::types::block::{Block, Header};

pub struct FakeBlockFetcher {}

#[async_trait]
impl BlockFetcher for FakeBlockFetcher {
    async fn latest_block(&self) -> Result<Block> {
        let header = Header {
            id: Default::default(),
            da_height: 7,
            transactions_count: 0,
            message_receipt_count: 0,
            transactions_root: Default::default(),
            message_receipt_root: Default::default(),
            height: 7,
            prev_root: Default::default(),
            time: None,
            application_hash: Default::default(),
        };

        Ok(Block {
            id: Default::default(),
            header,
            transactions: vec![],
        })
    }
}
