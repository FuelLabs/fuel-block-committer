use std::{
    net::Ipv4Addr,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    str::FromStr,
    time::Duration,
};

use clap::{command, Parser};
use eth::{Address, L1Keys};
use fuel_block_committer_encoding::bundle::CompressionLevel;
use serde::Deserialize;
use services::state_committer::{AlgoConfig, FeeMultiplierRange, FeeThresholds, SmaPeriods};
use storage::DbConfig;
use url::Url;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub eth: Eth,
    pub fuel: Fuel,
    pub app: App,
}

impl Config {
    pub fn fee_algo_config(&self) -> AlgoConfig {
        self.validated_fee_algo_config()
            .expect("already validated via `validate` in main")
    }

    pub fn validate(&self) -> crate::errors::Result<()> {
        let keys = &self.eth.l1_keys;
        if keys
            .blob
            .as_ref()
            .is_some_and(|blob_key| blob_key == &keys.main)
        {
            return Err(crate::errors::Error::Other(
                "Wallet key and blob pool wallet key must be different".to_string(),
            ));
        }

        if self.app.bundle.fragments_to_accumulate.get() > 6 {
            return Err(crate::errors::Error::Other(
                "Fragments to accumulate must be <= 6".to_string(),
            ));
        }

        if let Err(e) = self.validated_fee_algo_config() {
            return Err(crate::errors::Error::Other(format!(
                "Invalid fee algo config: {e}",
            )));
        }

        Ok(())
    }

    fn validated_fee_algo_config(&self) -> crate::errors::Result<AlgoConfig> {
        let config = self;
        let algo_config = services::state_committer::AlgoConfig {
            sma_periods: SmaPeriods {
                short: config.app.fee_algo.short_sma_blocks,
                long: config.app.fee_algo.long_sma_blocks,
            },
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: config.app.fee_algo.max_l2_blocks_behind,
                multiplier_range: FeeMultiplierRange::new(
                    config.app.fee_algo.start_max_fee_multiplier,
                    config.app.fee_algo.end_max_fee_multiplier,
                )?,
                always_acceptable_fee: config.app.fee_algo.always_acceptable_fee as u128,
            },
        };
        Ok(algo_config)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Fuel {
    /// URL to a fuel-core graphql endpoint.
    #[serde(deserialize_with = "parse_url")]
    pub graphql_endpoint: Url,
    pub num_buffered_requests: NonZeroU32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Eth {
    /// L1 keys for calling the state contract and for posting state
    pub l1_keys: L1Keys,
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

#[allow(dead_code)]
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
    /// How often to check for l1 fees
    #[serde(deserialize_with = "human_readable_duration")]
    pub l1_fee_check_interval: Duration,
    /// Number of L1 blocks that need to pass to accept the tx as finalized
    pub num_blocks_to_finalize_tx: u64,
    /// Interval after which to bump a pending tx
    #[serde(deserialize_with = "human_readable_duration")]
    pub gas_bump_timeout: Duration,
    /// Max gas fee we permit a tx to have in wei
    pub tx_max_fee: u64,
    ///// Contains configs relating to block state posting to l1
    pub bundle: BundleConfig,
    //// Duration for timeout when sending tx requests
    #[serde(deserialize_with = "human_readable_duration")]
    pub send_tx_request_timeout: Duration,
    //// Retention duration for state pruner
    #[serde(deserialize_with = "human_readable_duration")]
    pub state_pruner_retention: Duration,
    /// How often to run state pruner
    #[serde(deserialize_with = "human_readable_duration")]
    pub state_pruner_run_interval: Duration,
    /// Configuration for the fee algorithm used by the StateCommitter
    pub fee_algo: FeeAlgoConfig,
}

/// Configuration for the fee algorithm used by the StateCommitter
#[derive(Debug, Clone, Deserialize)]
pub struct FeeAlgoConfig {
    /// Short-term period for Simple Moving Average (SMA) in block numbers
    pub short_sma_blocks: NonZeroU64,

    /// Long-term period for Simple Moving Average (SMA) in block numbers
    pub long_sma_blocks: NonZeroU64,

    /// Maximum number of unposted L2 blocks before sending a transaction regardless of fees
    pub max_l2_blocks_behind: NonZeroU32,

    /// Starting multiplier applied when we're 0 l2 blocks behind
    pub start_max_fee_multiplier: f64,

    /// Ending multiplier applied if we're max_l2_blocks_behind - 1 blocks behind
    pub end_max_fee_multiplier: f64,

    /// A fee that is always acceptable regardless of other conditions
    pub always_acceptable_fee: u64,
}

/// Configuration settings for managing fuel block bundling and fragment submission operations.
///
/// This struct defines how blocks and fragments are accumulated, optimized, and eventually submitted to L1.
#[derive(Debug, Clone, Deserialize)]
pub struct BundleConfig {
    /// Duration to wait for additional fuel blocks before initiating the bundling process.
    ///
    /// This timeout is measured from the moment the last blob transaction was finalized, or, if
    /// missing, from the application startup time.
    #[serde(deserialize_with = "human_readable_duration")]
    pub accumulation_timeout: Duration,

    /// TODO: rephrase
    /// The number of bytes from fuel blocks to accumulate before initiating the bundling process.
    ///
    /// If the system successfully accumulates this number of bytes before the `accumulation_timeout` is reached,
    /// the bundling process will start immediately. Otherwise, the bundling process will be triggered when the
    /// `accumulation_timeout` fires, regardless of the number of bytes accumulated.
    pub bytes_to_accumulate: NonZeroUsize,

    /// TODO: rephrase
    pub blocks_to_accumulate: NonZeroUsize,

    /// TODO: rephrase
    /// target number of fragments the resulting bundle can be.
    pub target_fragments_per_bundle: NonZeroUsize,

    /// Maximum duration allocated for determining the optimal bundle size.
    ///
    /// This timeout limits the amount of time the system can spend searching for the ideal
    /// number of fuel blocks to include in a bundle. Once this duration is reached, the
    /// bundling process will proceed with the best configuration found within the allotted time.
    #[serde(deserialize_with = "human_readable_duration")]
    pub optimization_timeout: Duration,

    /// How big should the optimization step be at the start of the optimization process. Setting
    /// this value to 100 and giving the bundler a 1000 blocks would result in the following
    /// attempts:
    /// 1000, 900, ..., 100, 1, 950, 850, ..., 50, 975, 925, ...
    pub optimization_step: NonZeroUsize,

    /// Duration to wait for additional fragments before submitting them in a transaction to L1.
    ///
    /// Similar to `accumulation_timeout`, this timeout starts from the last finalized fragment submission. If no new
    /// fragments are received within this period, the system will proceed to submit the currently accumulated fragments.
    #[serde(deserialize_with = "human_readable_duration")]
    pub fragment_accumulation_timeout: Duration,

    /// The number of fragments to accumulate before submitting them in a transaction to L1.
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

    /// Compression level used for compressing block data before submission.
    ///
    /// Compression is applied to the blocks to minimize the transaction size. Valid values are:
    /// - `"disabled"`: No compression is applied.
    /// - `"min"` to `"max"`: Compression levels where higher numbers indicate more aggressive compression.
    pub compression_level: CompressionLevel,

    /// Duration to wait before checking if a new bundle can be made
    #[serde(deserialize_with = "human_readable_duration")]
    pub new_bundle_check_interval: Duration,
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
    pub eth_errors_before_unhealthy: usize,
    pub balance_update_interval: Duration,
    pub cost_request_limit: usize,
    pub l1_blocks_cached_for_fee_metrics_tracker: usize,
}

impl Default for Internal {
    fn default() -> Self {
        const ETH_BLOCK_TIME: usize = 12;
        const ETH_BLOCKS_PER_DAY: usize = 24 * 3600 / ETH_BLOCK_TIME;
        Self {
            fuel_errors_before_unhealthy: 3,
            eth_errors_before_unhealthy: 3,
            balance_update_interval: Duration::from_secs(10),
            cost_request_limit: 1000,
            l1_blocks_cached_for_fee_metrics_tracker: ETH_BLOCKS_PER_DAY,
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
struct Cli {
    #[arg(
        value_name = "FILE",
        help = "Used to be the path to the configuration, unused currently until helm charts are updated."
    )]
    config_path: Option<String>,
}

pub fn parse() -> crate::errors::Result<Config> {
    let _ = Cli::parse();

    let config = config::Config::builder()
        .add_source(config::Environment::with_prefix("COMMITTER").separator("__"))
        .build()?;

    Ok(config.try_deserialize()?)
}
