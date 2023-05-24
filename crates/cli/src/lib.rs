use clap::{command, Parser};
use fuel_types::{Bytes20, Bytes32};
use url::Url;

const ETHEREUM_RPC: &str = "https://mainnet.infura.io/v3/YOUR_PROJECT_ID";
const FUEL_GRAPHQL_ENDPOINT: &str = "https://fuelgraphqlendpoint/TODO";
const STATE_CONTRACT_ADDRESS: Bytes20 = Bytes20::zeroed();
const COMMIT_INTERVAL: u32 = 1;

pub struct Config {
    pub ethereum_wallet_key: Bytes32,
    pub ethereum_rpc: Url,
    pub fuel_graphql_endpoint: Url,
    pub state_contract_address: Bytes20,
    pub commit_interval: u32,
}

#[derive(Parser)]
#[command(
    name = "fuel-block-committer",
    version,
    about,
    propagate_version = true
)]
pub struct Cli {
    /// Ethereum wallet key
    #[arg(long, value_name = "BYTES32")]
    ethereum_wallet_key: Bytes32,

    /// Ethereum RPC
    #[arg(long, default_value = ETHEREUM_RPC, value_name = "URL")]
    ethereum_rpc: Url,

    /// Fuel GraphQL endpoint
    #[arg(short, long, default_value = FUEL_GRAPHQL_ENDPOINT, value_name = "URL")]
    fuel_graphql_endpoint: Url,

    /// State contract address
    #[arg(long, default_value_t = STATE_CONTRACT_ADDRESS, value_name = "BYTES20")]
    state_contract_address: Bytes20,

    /// Commit interval
    #[clap(long, default_value_t = COMMIT_INTERVAL, value_name = "U32")]
    commit_interval: u32,
}

pub fn parse_cli() -> Config {
    let cli = Cli::parse();

    Config {
        ethereum_wallet_key: cli.ethereum_wallet_key,
        ethereum_rpc: cli.ethereum_rpc,
        fuel_graphql_endpoint: cli.fuel_graphql_endpoint,
        state_contract_address: cli.state_contract_address,
        commit_interval: cli.commit_interval,
    }
}
