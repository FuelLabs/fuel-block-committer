use clap::{command, Parser};

pub const ETHEREUM_RPC: &str = "Elvis";
pub const FUEL_GRAPHQL_ENDPOINT: &str = "Dedic";
pub const STATE_CONTRACT_ADDRESS: &str = "Leviatan";
pub const COMMIT_INTERVAL: u32 = 1;

#[derive(Parser, Debug)]
#[command(
    name = "fuel-block-committer-cli",
    version,
    about,
    propagate_version = true
)]
pub struct Cli {
    /// Ethereum wallet key
    #[clap(long)]
    ethereum_wallet_key: String,

    /// Ethereum RPC
    #[clap(long, default_value = ETHEREUM_RPC)]
    ethereum_rpc: String,

    /// Fuel GraphQL endpoint
    #[clap(long, default_value = FUEL_GRAPHQL_ENDPOINT)]
    fuel_graphql_endpoint: String,

    /// State contract address
    #[clap(long, default_value = STATE_CONTRACT_ADDRESS)]
    state_contract_address: String,

    /// Commit interval
    #[clap(long, default_value_t = COMMIT_INTERVAL)]
    commit_interval: u32,
}

pub async fn run_cli() {
    let cli = Cli::parse();
    println!("{:?}", cli.ethereum_wallet_key);
    println!("{:?}", cli.ethereum_rpc);
    println!("{:?}", cli.fuel_graphql_endpoint);
    println!("{:?}", cli.state_contract_address);
    println!("{:?}", cli.commit_interval);
}
