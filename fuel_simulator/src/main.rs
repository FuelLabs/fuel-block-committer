mod server;

use clap::Parser;
use server::run_server;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// block size in bytes for each produced block.
    #[arg(long, default_value_t = 128)]
    block_size: usize,

    /// port to run the server on.
    #[arg(long, default_value_t = 4000)]
    port: u16,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    run_server(args.block_size, args.port).await;
}
