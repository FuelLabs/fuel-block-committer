use std::{
    net::Ipv4Addr,
    num::{NonZeroU32, NonZeroUsize},
    str::FromStr,
    time::Duration,
};

use clap::{command, Parser};
use eth::Address;
use serde::Deserialize;
use services::CompressionLevel;
use storage::DbConfig;
use url::Url;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub eth: Eth,
    pub fuel: Fuel,
    pub app: App,
}

impl Config {
    pub fn validate(&self) -> crate::errors::Result<()> {
        if let Some(blob_pool_wallet_key) = &self.eth.blob_pool_key_arn {
            if blob_pool_wallet_key == &self.eth.main_key_arn {
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
    pub main_key_arn: String,
    /// The AWS KMS key ID for posting L2 state to L1.
    pub blob_pool_key_arn: Option<String>,
    /// URL to a Ethereum RPC endpoint.
    #[serde(deserialize_with = "parse_url")]
    pub rpc: Url,
    /// Ethereum address of the fuel chain state contract.
    pub state_contract_address: Address,
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
pub struct App {
    /// Port used by the started server
    pub port: u16,
    /// IPv4 address on which the server will listen for connections
    pub host: Ipv4Addr,
    /// Postgres database configuration
    pub db: DbConfig,
    /// How often to check for fuel blocks
    #[serde(deserialize_with = "human_readable_duration")]
    pub block_check_interval: Duration,
    /// How often to check for finalized l1 txs
    #[serde(deserialize_with = "human_readable_duration")]
    pub tx_finalization_check_interval: Duration,
    /// Number of L1 blocks that need to pass to accept the tx as finalized
    pub num_blocks_to_finalize_tx: u64,
    ///// Contains configs relating to block state posting to l1
    pub bundle: BundleConfig,
}

/// Configuration settings for managing fuel block bundling operations.
///
/// This struct encapsulates various timeouts and window settings that govern
/// how fuel blocks are accumulated, optimized, and submitted to L1.
#[derive(Debug, Clone, Deserialize)]
pub struct BundleConfig {
    /// Duration to wait for additional fuel blocks before initiating the bundling process.
    ///
    /// This timeout is measured from the moment the last blob transaction was finalized, or, if
    /// missing, from the application startup time.
    ///
    /// If no new fuel blocks are received within this period, the current set of accumulated
    /// blocks will be bundled.
    #[serde(deserialize_with = "human_readable_duration")]
    pub accumulation_timeout: Duration,

    /// The number of fuel blocks to accumulate before initiating the bundling process.
    ///
    /// If the system successfully accumulates this number of blocks before the `accumulation_timeout` is reached,
    /// the bundling process will start immediately. Otherwise, the bundling process will be triggered when the
    /// `accumulation_timeout` fires, regardless of the number of blocks accumulated.
    pub blocks_to_accumulate: NonZeroUsize,

    /// Maximum duration allocated for determining the optimal bundle size.
    ///
    /// This timeout limits the amount of time the system can spend searching for the ideal
    /// number of fuel blocks to include in a bundle. Once this duration is reached, the
    /// bundling process will proceed with the best configuration found within the allotted time.
    #[serde(deserialize_with = "human_readable_duration")]
    pub optimization_timeout: Duration,

    // TODO: segfault
    pub optimization_step: NonZeroUsize,

    // TODO: segfault
    #[serde(deserialize_with = "human_readable_duration")]
    pub fragment_accumulation_timeout: Duration,

    // TODO: segfault
    pub fragments_to_accumulate: NonZeroUsize,

    /// Only blocks within the `block_height_lookback` window
    /// value will be considered for importing, bundling, fragmenting, and submitting to L1.
    ///
    /// This parameter defines a sliding window based on block height to determine which blocks are
    /// eligible for processing. Specifically:
    ///
    /// - **Exclusion of Stale Blocks:** If a block arrives with a height less than the current
    ///   height minus the `block_height_lookback`, it will be excluded from the bundling process.
    ///
    /// - **Bundling Behavior:**
    ///   - **Unbundled Blocks:** Blocks outside the lookback window will not be bundled.
    ///   - **Already Bundled Blocks:** If a block has already been bundled, its fragments will
    ///     not be sent to L1.
    ///   - **Failed Submissions:** If fragments of a bundled block were sent to L1 but failed,
    ///     they will not be retried.
    ///
    /// This approach effectively "gives up" on blocks that fall outside the defined window.
    pub block_height_lookback: u32,

    /// Valid values: "disabled", "min", "1" to "9", "max"
    pub compression_level: CompressionLevel,
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
    pub new_bundle_check_interval: Duration,
    pub max_full_blocks_per_request: NonZeroU32,
}

impl Default for Internal {
    fn default() -> Self {
        Self {
            fuel_errors_before_unhealthy: 3,
            between_eth_event_stream_restablishing_attempts: Duration::from_secs(3),
            eth_errors_before_unhealthy: 3,
            balance_update_interval: Duration::from_secs(10),
            new_bundle_check_interval: Duration::from_secs(10),
            max_full_blocks_per_request: 100.try_into().unwrap(),
        }
    }
}
#[derive(Parser)]
#[command(
    name = "fuel-block-committer",
    version,
    about,
    propagate_version = true
)]
struct Cli {}

pub fn parse() -> crate::errors::Result<Config> {
    let _ = Cli::parse();

    let config = config::Config::builder()
        .add_source(config::Environment::with_prefix("COMMITTER").separator("__"))
        .build()?;

    Ok(config.try_deserialize()?)
}
