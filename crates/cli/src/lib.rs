use clap::error::ErrorKind;
use clap::{command, Command, Parser};
use fuel_types::{Bytes20, Bytes32};
use std::str::FromStr;
use url::Url;

const ETHEREUM_RPC: &str = "https://mainnet.infura.io/v3/YOUR_PROJECT_ID";
const FUEL_GRAPHQL_ENDPOINT: &str = "https://127.0.0.1:4000";
const STATE_CONTRACT_ADDRESS: Bytes20 = Bytes20::zeroed();
const COMMIT_INTERVAL: u32 = 1;

#[derive(Debug, Clone)]
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
#[derive(Debug)]
struct Cli {
    /// Ethereum wallet key
    #[arg(
        long,
        value_name = "BYTES32",
        help = "The secret key authorized by the L1 bridging contracts to post block commitments.",
        required = false
    )]
    ethereum_wallet_key: Option<Bytes32>,

    /// Ethereum RPC
    #[arg(long, default_value = ETHEREUM_RPC, value_name = "URL", help = "URL to a Ethereum RPC endpoint.")]
    ethereum_rpc: Url,

    /// Fuel GraphQL endpoint
    #[arg(short, long, default_value = FUEL_GRAPHQL_ENDPOINT, value_name = "URL", help = "URL to a fuel-core graphql endpoint.")]
    fuel_graphql_endpoint: Url,

    /// State contract address
    #[arg(long, default_value_t = STATE_CONTRACT_ADDRESS, value_name = "BYTES20", help = "Ethereum address of the fuel chain state contract.")]
    state_contract_address: Bytes20,

    /// Commit interval
    #[clap(long, default_value_t = COMMIT_INTERVAL, value_name = "U32", help = "The number of fuel blocks between ethereum commits. If set to 1, then every block should be pushed to Ethereum.")]
    commit_interval: u32,
}

pub fn parse_cli() -> Config {
    let cli = Cli::parse();

    let ethereum_wallet_key = std::env::var("ETHEREUM_WALLET_KEY")
        .ok()
        .and_then(|value| Bytes32::from_str(&value).ok())
        .or(cli.ethereum_wallet_key);

    if ethereum_wallet_key.is_none() {
        Command::error(
            &mut Command::new("fuel-block-committer-bin --ethereum-wallet-key <BYTES32>"),
                ErrorKind::MissingRequiredArgument,
            "Please provide the Ethereum wallet key using the \x1B[33m'--ethereum-wallet-key'\x1B[0m command-line argument or set the \x1B[33m'ETHEREUM_WALLET_KEY'\x1B[0m environmental variable."
        ).exit();
    }

    Config {
        ethereum_wallet_key: ethereum_wallet_key.unwrap(),
        ethereum_rpc: Url::parse(
            &std::env::var("ETHEREUM_RPC").unwrap_or(cli.ethereum_rpc.to_string()),
        )
        .unwrap(),
        fuel_graphql_endpoint: Url::parse(
            &std::env::var("FUEL_GRAPHQL_ENDPOINT")
                .unwrap_or(cli.fuel_graphql_endpoint.to_string()),
        )
        .unwrap(),
        state_contract_address: Bytes20::from_str(
            &std::env::var("STATE_CONTRACT_ADDRESS")
                .unwrap_or(cli.state_contract_address.to_string()),
        )
        .unwrap(),
        commit_interval: std::env::var("COMMIT_INTERVAL")
            .unwrap_or(cli.commit_interval.to_string())
            .parse()
            .unwrap(),
    }
}
