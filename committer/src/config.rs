use std::{net::Ipv4Addr, num::NonZeroU32, path::PathBuf, str::FromStr, time::Duration};

use clap::{command, Parser};
use eth_rpc::{Address, Chain};
use serde::Deserialize;
use storage::DbConfig;
use url::Url;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub eth: EthConfig,
    pub fuel: FuelConfig,
    pub app: AppConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FuelConfig {
    /// URL to a fuel-core graphql endpoint.
    #[serde(deserialize_with = "parse_url")]
    pub graphql_endpoint: Url,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EthConfig {
    /// The secret key authorized by the L1 bridging contracts to post block commitments.
    pub wallet_key: String,
    /// URL to a Ethereum RPC endpoint.
    #[serde(deserialize_with = "parse_url")]
    pub rpc: Url,
    /// Chain id of the ethereum network.
    #[serde(deserialize_with = "parse_chain_id")]
    pub chain_id: Chain,
    /// Ethereum address of the fuel chain state contract.
    pub state_contract_address: Address,
    /// The number of fuel blocks between ethereum commits. If set to 1, then every block should be pushed to Ethereum.
    pub commit_interval: NonZeroU32,
}

fn parse_chain_id<'de, D>(deserializer: D) -> Result<Chain, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let chain_id: String = Deserialize::deserialize(deserializer)?;
    Chain::from_str(&chain_id).map_err(|_| {
        let msg = format!("Failed to parse chain id '{chain_id}'");
        serde::de::Error::custom(msg)
    })
}

fn parse_url<'de, D>(deserializer: D) -> Result<Url, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let url_str: String = Deserialize::deserialize(deserializer).unwrap();
    Url::from_str(&url_str).map_err(|e| {
        let msg = format!("Failed to parse URL '{url_str}': {e};");
        serde::de::Error::custom(msg)
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    /// Port used by the started server
    pub port: u16,
    /// IPv4 address on which the server will listen for connections
    pub host: Ipv4Addr,
    /// Postgres database configuration
    /// TODO: don't rely on this type it is not robust
    pub db: DbConfig,
}

#[derive(Debug, Clone)]
pub struct InternalConfig {
    pub fuel_polling_interval: Duration,
    pub fuel_errors_before_unhealthy: usize,
    pub between_eth_event_stream_restablishing_attempts: Duration,
    pub eth_errors_before_unhealthy: usize,
    pub balance_update_interval: Duration,
}

impl Default for InternalConfig {
    fn default() -> Self {
        Self {
            fuel_polling_interval: Duration::from_secs(3),
            fuel_errors_before_unhealthy: 3,
            between_eth_event_stream_restablishing_attempts: Duration::from_secs(3),
            eth_errors_before_unhealthy: 3,
            balance_update_interval: Duration::from_secs(10),
        }
    }
}
#[derive(Parser)]
#[command(
    name = "fuel-block-committer",
    version,
    about,
    propagate_version = true,
    arg_required_else_help(true)
)]
struct Cli {
    #[arg(value_name = "FILE", help = "Path to the configuration file")]
    config_path: PathBuf,
}

pub fn parse() -> crate::errors::Result<Config> {
    let cli = Cli::parse();

    let config = config::Config::builder()
        .add_source(config::File::from(cli.config_path))
        .add_source(config::Environment::with_prefix("COMMITTER").separator("__"))
        .build()?;

    Ok(config.try_deserialize()?)
}
