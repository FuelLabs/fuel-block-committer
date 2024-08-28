use std::{net::Ipv4Addr, path::PathBuf, str::FromStr, time::Duration};

use clap::{command, Parser};
use eth::{Address, Chain};
use serde::Deserialize;
use storage::DbConfig;
use url::Url;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub eth: Eth,
    pub fuel: Fuel,
    pub app: App,
    pub aws: Aws,
}

impl Config {
    pub fn validate(&self) -> crate::errors::Result<()> {
        if let Some(blob_pool_wallet_key) = &self.eth.blob_pool_key_id {
            if blob_pool_wallet_key == &self.eth.main_key_id {
                return Err(crate::errors::Error::Other(
                    "Wallet key and blob pool wallet key must be different".to_string(),
                ));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Fuel {
    /// URL to a fuel-core graphql endpoint.
    #[serde(deserialize_with = "parse_url")]
    pub graphql_endpoint: Url,
    /// Block producer address
    pub block_producer_address: ports::fuel::FuelBytes32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Eth {
    /// The AWS KMS key ID authorized by the L1 bridging contracts to post block commitments.
    pub main_key_id: String,
    /// The AWS KMS key ID for posting L2 state to L1.
    pub blob_pool_key_id: Option<String>,
    /// URL to a Ethereum RPC endpoint.
    #[serde(deserialize_with = "parse_url")]
    pub rpc: Url,
    /// Chain id of the ethereum network.
    #[serde(deserialize_with = "parse_chain_id")]
    pub chain_id: Chain,
    /// Ethereum address of the fuel chain state contract.
    pub state_contract_address: Address,
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
pub struct Aws {
    pub allow_http: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct App {
    /// Port used by the started server
    pub port: u16,
    /// IPv4 address on which the server will listen for connections
    pub host: Ipv4Addr,
    /// Postgres database configuration
    pub db: DbConfig,
    /// How often to check the latest fuel block
    #[serde(deserialize_with = "human_readable_duration")]
    pub block_check_interval: Duration,
    /// Number of L1 blocks that need to pass to accept the tx as finalized
    pub num_blocks_to_finalize_tx: u64,
}

fn human_readable_duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let duration_str: String = Deserialize::deserialize(deserializer).unwrap();
    humantime::parse_duration(&duration_str).map_err(|e| {
        let msg = format!("Failed to parse duration '{duration_str}': {e};");
        serde::de::Error::custom(msg)
    })
}

#[derive(Debug, Clone)]
pub struct Internal {
    pub fuel_errors_before_unhealthy: usize,
    pub between_eth_event_stream_restablishing_attempts: Duration,
    pub eth_errors_before_unhealthy: usize,
    pub balance_update_interval: Duration,
}

impl Default for Internal {
    fn default() -> Self {
        Self {
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
