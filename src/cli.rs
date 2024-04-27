use std::{net::Ipv4Addr, num::NonZeroU32, path::PathBuf};

use clap::{command, Parser};
use ethers::types::{Address, Chain};
use url::Url;

use crate::{
    adapters::storage::postgresql::ConnectionOptions,
    errors::{Error, Result},
    setup::config::{CommitterConfig, Config, EthConfig, FuelConfig},
};

const ETHEREUM_RPC: &str = "ws://127.0.0.1:8545/";
const FUEL_GRAPHQL_ENDPOINT: &str = "http://127.0.0.1:4000";
const COMMIT_INTERVAL: u32 = 1;
const HOST: &str = "127.0.0.1";
const PORT: u16 = 8080;
const CHAIN_NAME: Chain = Chain::Mainnet;

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
        value_name = "String",
        help = "The secret key authorized by the L1 bridging contracts to post block commitments."
    )]
    ethereum_wallet_key: String,

    /// Ethereum RPC
    #[arg(
        long,
        env = "ETHEREUM_RPC",
        default_value = ETHEREUM_RPC, value_name = "URL", help = "URL to a Ethereum RPC endpoint."
    )]
    ethereum_rpc: Url,

    /// Fuel GraphQL endpoint
    #[arg(
        long,
        env = "FUEL_GRAPHQL_ENDPOINT",
        default_value = FUEL_GRAPHQL_ENDPOINT, value_name = "URL", help = "URL to a fuel-core graphql endpoint."
    )]
    fuel_graphql_endpoint: Url,

    /// State contract address
    #[arg(
        long,
        env = "STATE_CONTRACT_ADDRESS",
        value_name = "Address",
        help = "Ethereum address of the fuel chain state contract."
    )]
    state_contract_address: Address,

    /// Commit interval
    #[arg(
        long,
        env = "COMMIT_INTERVAL",
        default_value_t = COMMIT_INTERVAL, value_name = "U32", help = "The number of fuel \\
        blocks between ethereum commits. If set to 1, then every block should be pushed to Ethereum."
    )]
    commit_interval: u32,

    /// Host of the started API server
    #[arg(
        long,
        env = "HOST",
        default_value = HOST,
        value_name = "IPv4",
        help = "Host on which to start the API server."
    )]
    host: Ipv4Addr,

    /// Port of the started API server
    #[arg(
        long,
        env = "PORT",
        default_value_t = PORT, value_name = "U16", help = "Port on which to start the API server."
    )]
    port: u16,

    /// Ethereum chain id
    #[arg(
        long,
        env = "ETHEREUM_CHAIN",
        default_value_t = CHAIN_NAME, value_name = "ChainName", help = "Chain id of the ethereum network."
    )]
    ethereum_chain: Chain,

    #[arg(
        long,
        env = "DB_PATH",
        value_name = "Path",
        help = "Path to db storage. If not given will use temporary storage."
    )]
    db_path: Option<PathBuf>,
}

pub fn parse() -> Result<Config> {
    let cli = Cli::parse();
    let commit_interval = NonZeroU32::new(cli.commit_interval)
        .ok_or_else(|| Error::Other("Commit interval must not be 0!".to_string()))?;
    Ok(Config {
        eth: EthConfig {
            ethereum_wallet_key: cli.ethereum_wallet_key,
            ethereum_rpc: cli.ethereum_rpc,
            ethereum_chain_id: cli.ethereum_chain,
            state_contract_address: cli.state_contract_address,
        },
        fuel: FuelConfig {
            fuel_graphql_endpoint: cli.fuel_graphql_endpoint,
        },
        committer: CommitterConfig {
            commit_interval,
            port: cli.port,
            host: cli.host,
            db: ConnectionOptions {
                host: todo!(),
                port: todo!(),
                username: todo!(),
                password: todo!(),
                db: todo!(),
                connections: todo!(),
            },
        },
    })
}
