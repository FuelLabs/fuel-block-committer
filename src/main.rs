use std::time::Duration;

use actix::Actor;
use actors::block_watcher::BlockWatcher;
use adapters::block_fetcher::FuelBlockFetcher;

mod actors;
mod adapters;
mod cli;
mod errors;

#[actix::main]
async fn main() {
    let bw = BlockWatcher::new(Duration::from_millis(500), FuelBlockFetcher {}).start();

    let _: () = std::future::pending().await;
}
