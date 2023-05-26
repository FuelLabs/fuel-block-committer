use crate::errors::Error;
use crate::errors::Result;
use crate::setup::config::Config;
use clap::{command, Parser};
use fuels::{accounts::fuel_crypto::fuel_types::Bytes20, tx::Bytes32};
use url::Url;

const ETHEREUM_RPC: &str = "https://mainnet.infura.io/v3/YOUR_PROJECT_ID";
const FUEL_GRAPHQL_ENDPOINT: &str = "https://127.0.0.1:4000";
const STATE_CONTRACT_ADDRESS: Bytes20 = Bytes20::zeroed();
const COMMIT_INTERVAL: u32 = 1;

#[derive(Parser)]
#[command(
    name = "fuel-block-committer",
    version,
    about,
    propagate_version = true
)]
struct Cli {
    /// Ethereum wallet key
    #[arg(
        long,
        env = "ETHEREUM_WALLET_KEY",
        value_name = "BYTES32",
        help = "The secret key authorized by the L1 bridging contracts to post block commitments."
    )]
    ethereum_wallet_key: Bytes32,

    /// Ethereum RPC
    #[arg(long,
    env = "ETHEREUM_RPC",
    default_value = ETHEREUM_RPC, value_name = "URL", help = "URL to a Ethereum RPC endpoint.")]
    ethereum_rpc: Url,

    /// Fuel GraphQL endpoint
    #[arg(long,
    env = "FUEL_GRAPHQL_ENDPOINT",
    default_value = FUEL_GRAPHQL_ENDPOINT, value_name = "URL", help = "URL to a fuel-core graphql endpoint.")]
    fuel_graphql_endpoint: Url,

    /// State contract address
    #[arg(long,
    env = "STATE_CONTRACT_ADDRESS",
    default_value_t = STATE_CONTRACT_ADDRESS, value_name = "BYTES20", help = "Ethereum address of the fuel chain state contract.")]
    state_contract_address: Bytes20,

    /// Commit interval
    #[arg(long,
    env = "COMMIT_INTERVAL",
    default_value_t = COMMIT_INTERVAL, value_name = "U32", help = "The number of fuel blocks between ethereum commits. If set to 1, then every block should be pushed to Ethereum.")]
    commit_interval: u32,
}

pub fn parse() -> Result<Config> {
    let cli = Cli::try_parse().map_err(|e| Error::Other(e.to_string()))?;
    Ok(Config {
        ethereum_wallet_key: cli.ethereum_wallet_key,
        ethereum_rpc: cli.ethereum_rpc,
        fuel_graphql_endpoint: cli.fuel_graphql_endpoint,
        state_contract_address: cli.state_contract_address,
        commit_epoch: cli.commit_interval,
    })
}
