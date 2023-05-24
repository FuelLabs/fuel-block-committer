use std::time::Duration;

use actix::Actor;
use actors::{block_watcher::BlockWatcher, eth_committer::EthCommitter};
use adapters::{
    block_fetcher::FakeBlockFetcher, storage::FakeStorage, tx_status::FakeTxStatusProvider,
    tx_submitter::FakeTxSubmitter,
};

mod actors;
mod adapters;
mod cli;
mod common;
mod errors;

#[actix::main]
async fn main() {
    let committer = EthCommitter::new(
        Duration::from_secs(2),
        FakeTxStatusProvider {},
        FakeTxSubmitter {},
        FakeStorage {},
    )
    .start();

    let _block_watcher = BlockWatcher::new(
        Duration::from_secs(10),
        FakeBlockFetcher {},
        committer.into(),
    )
    .start();

    std::future::pending::<()>().await;
}
