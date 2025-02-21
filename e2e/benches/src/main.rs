use std::time::Duration;

use clap::Parser;
use fuel::{HttpClient, Url};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, default_value_t = 128)]
    block_size: usize,

    #[arg(long, default_value_t = 8000)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "debug");
    }

    env_logger::init();
    let args = Args::parse();

    let mut server = e2e_helpers::fuel_node_simulated::GraphQLServer::new(args.port);

    server.run(args.block_size);

    tokio::time::sleep(Duration::from_secs(2)).await;

    let client = HttpClient::new(&server.url(), 100, 1.try_into().unwrap());

    let block = client.latest_block().await?;
    let da_block = client
        .compressed_block_at_height(block.height)
        .await?
        .unwrap();
    let block_at_height = client.block_at_height(block.height).await?.unwrap();

    Ok(())
}
