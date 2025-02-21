mod server;

use clap::Parser;
use server::run_server;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, default_value_t = 128)]
    block_size: usize,

    #[arg(long, default_value_t = 8000)]
    port: u16,
}

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "debug");
    }

    env_logger::init();
    let args = Args::parse();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run_server(args.block_size, args.port));
}
