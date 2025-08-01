use std::{
    net::Ipv4Addr,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    str::FromStr,
    time::Duration,
};

use byte_unit::Byte;
use clap::{Parser, command};
use eth::{Address, L1Signers};
use fuel_block_committer_encoding::bundle::CompressionLevel;
use serde::Deserialize;
use services::state_committer::{AlgoConfig, FeeMultiplierRange, FeeThresholds, SmaPeriods};
use signers::{aws_sdk_kms::Client, kms_utils::load_config_from_env};
use storage::DbConfig;
use url::Url;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub eth: Eth,
    pub da_layer: Option<DALayer>,
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

        if self.app.bundle.block_height_lookback < self.app.bundle.blocks_to_accumulate.get() as u32
        {
            return Err(crate::errors::Error::Other(
                "block_height_lookback must be >= blocks_to_accumulate".to_string(),
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
    /// Fuel-core GraphQL endpoint URL.
    #[serde(deserialize_with = "parse_url")]
    pub graphql_endpoint: Url,
    /// Number of concurrent requests.
    pub num_buffered_requests: NonZeroU32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum DALayer {
    #[serde(rename = "EigenDA")]
    EigenDA(EigenDaConfig),
}

#[derive(Debug, Clone, Deserialize)]
pub struct EigenDaConfig {
    /// Key for posting authenticated requests to the EigenDA disperser.
    pub key: KeySource,
    /// URL to an EigenDA RPC endpoint.
    #[serde(deserialize_with = "parse_url")]
    pub disperser_rpc_url: Url,
    /// Blob fragment size.
    /// Defaults to 3.5MB, as 4+MB errors out on the server side when checking for inclusion.
    pub fragment_size: Option<NonZeroU32>,
    /// Fee check interval.
    #[serde(deserialize_with = "human_readable_duration")]
    pub fee_check_interval: Duration,
    /// Polling interval.
    /// Defaults to 1s if not given.
    #[serde(deserialize_with = "maybe_human_readable_duration")]
    pub polling_interval: Option<Duration>,
    /// Allocated API throughput limit in bytes/s (for the address corresponding to the key).
    /// Defaults to 16 MiB/s if not given.
    #[serde(deserialize_with = "parse_u32")]
    pub api_throughput: Option<u32>,
    /// URL to the EigenDA RPC endpoint for Ethereum.
    #[serde(deserialize_with = "parse_url")]
    pub eth_rpc_url: Url,
    /// Certificate verifier address.
    pub cert_verifier_address: String,
    /// Registry coordinator address.
    pub registry_coordinator_address: String,
    /// Operator state retriever address.
    pub operator_state_retriever_address: String,
}

impl EigenDaConfig {
    pub async fn signer(&self) -> crate::errors::Result<signers::eigen::Signer> {
        let signer = match &self.key {
            KeySource::Private(key) => {
                let key = key.parse().map_err(|_| {
                    crate::errors::Error::Other("Failed to parse eigen private key".to_owned())
                })?;
                signers::eigen::Signer::Private(signers::eigen::private_key::Signer::new(key))
            }
            KeySource::Kms(key) => {
                let config = load_config_from_env().await;
                let client = Client::new(&config);
                let signer = signers::eigen::kms::Signer::new(client, key.clone())
                    .await
                    .map_err(|e| {
                        crate::errors::Error::Other(format!(
                            "Failed to create eigen kms signer: {e}"
                        ))
                    })?;
                signers::eigen::Signer::Kms(signer)
            }
        };
        Ok(signer)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Eth {
    /// L1 keys for state contract calls and postings.
    pub l1_keys: L1Keys,
    /// Ethereum RPC endpoint URL.
    #[serde(deserialize_with = "parse_url")]
    pub rpc: Url,
    /// Ethereum address of the fuel chain state contract.
    pub state_contract_address: Address,
}

impl Eth {
    pub async fn signers(
        &self,
    ) -> crate::errors::Result<L1Signers<signers::eth::Signer, signers::eth::Signer>> {
        // A little helper to turn a KeySource into a Signer, tagging errors appropriately.
        let make_signer = |ks: KeySource, label: &'static str| async move {
            match ks.clone() {
                KeySource::Kms(key) => {
                    let config = load_config_from_env().await;
                    let client = Client::new(&config);
                    let kms = signers::eth::kms::Signer::new(client, key.to_owned(), None)
                        .await
                        .map_err(|e| {
                            crate::errors::Error::Other(format!(
                                "Failed to create {} kms signer: {e}",
                                label
                            ))
                        })?;

                    crate::Result::<_>::Ok(signers::eth::Signer::Kms(kms))
                }
                KeySource::Private(key) => {
                    let pk = signers::eth::private_key::Signer::from_str(&key).map_err(|_| {
                        crate::errors::Error::Other(format!(
                            "Failed to create {} private key signer",
                            label
                        ))
                    })?;
                    Ok(signers::eth::Signer::Private(pk))
                }
            }
        };

        // Build the `main` signer
        let main = make_signer(self.l1_keys.main.clone(), "main").await?;

        // Build the optional `blob` signer
        let blob = if let Some(ref ks) = self.l1_keys.blob {
            Some(make_signer(ks.clone(), "blob").await?)
        } else {
            None
        };

        Ok(L1Signers { main, blob })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct L1Keys {
    pub main: KeySource,
    pub blob: Option<KeySource>,
}

#[derive(Clone, PartialEq)]
pub enum KeySource {
    Kms(String),
    Private(String),
}

impl std::fmt::Debug for KeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeySource::Kms(_) => write!(f, "KeySource::Kms"),
            KeySource::Private(_) => write!(f, "KeySource::Private"),
        }
    }
}

impl std::fmt::Display for KeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeySource::Kms(_) => write!(f, "Kms(...)"),
            KeySource::Private(_) => write!(f, "Private(...)"),
        }
    }
}

impl<'a> serde::Deserialize<'a> for KeySource {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        let value = String::deserialize(deserializer)?;
        if let Some(k) = value.strip_prefix("Kms(").and_then(|s| s.strip_suffix(')')) {
            Ok(KeySource::Kms(k.to_string()))
        } else if let Some(k) = value
            .strip_prefix("Private(")
            .and_then(|s| s.strip_suffix(')'))
        {
            Ok(KeySource::Private(k.to_string()))
        } else {
            Err(serde::de::Error::custom("invalid KeySource format"))
        }
    }
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

// Custom deserializer that handles string or number input for u32
fn parse_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Number(u32),
    }

    let value: Option<StringOrNumber> = Option::deserialize(deserializer)?;
    match value {
        Some(StringOrNumber::String(s)) => s.parse().map(Some).map_err(|e| {
            serde::de::Error::custom(format!("Failed to parse '{}' as u32: {}", s, e))
        }),
        Some(StringOrNumber::Number(n)) => Ok(Some(n)),
        None => Ok(None),
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct App {
    /// Server port.
    pub port: u16,
    /// Server IPv4 address.
    pub host: Ipv4Addr,
    /// Postgres database configuration.
    pub db: DbConfig,
    /// Interval for checking new fuel blocks.
    #[serde(deserialize_with = "human_readable_duration")]
    pub block_check_interval: Duration,
    /// Interval for checking finalized L1 transactions.
    #[serde(deserialize_with = "human_readable_duration")]
    pub tx_finalization_check_interval: Duration,
    /// Interval for checking L1 fees.
    #[serde(deserialize_with = "human_readable_duration")]
    pub l1_fee_check_interval: Duration,
    /// Number of L1 blocks to wait before finalizing a transaction.
    pub num_blocks_to_finalize_tx: u64,
    /// Timeout after which a pending transaction is bumped.
    #[serde(deserialize_with = "human_readable_duration")]
    pub gas_bump_timeout: Duration,
    /// Settings for L1 transaction fees.
    pub tx_fees: TxFeesConfig,
    /// Settings for bundling blocks.
    pub bundle: BundleConfig,
    /// Timeout for sending transaction requests.
    #[serde(deserialize_with = "human_readable_duration")]
    pub send_tx_request_timeout: Duration,
    /// Retention period for state pruner.
    #[serde(deserialize_with = "human_readable_duration")]
    pub state_pruner_retention: Duration,
    /// Interval for running the state pruner.
    #[serde(deserialize_with = "human_readable_duration")]
    pub state_pruner_run_interval: Duration,
    /// Configuration for the fee tracking algorithm.
    pub fee_algo: FeeAlgoConfig,
}

#[derive(Debug, Clone, Deserialize, Copy)]
pub struct TxFeesConfig {
    /// Maximum allowed gas fee in wei.
    pub max: u64,
    /// Minimum reward percentage when L2 block posting is current. (eg. 20%)
    pub min_reward_perc: f64,
    /// Maximum reward percentage when L2 block posting is maximally delayed. (eg. 30%)
    pub max_reward_perc: f64,
}

/// Fee algorithm configuration for the StateCommitter.
#[derive(Debug, Clone, Deserialize)]
pub struct FeeAlgoConfig {
    /// Short-term SMA period (in blocks).
    pub short_sma_blocks: NonZeroU64,
    /// Long-term SMA period (in blocks).
    pub long_sma_blocks: NonZeroU64,
    /// Maximum allowed lag (in unposted L2 blocks) before forcing a tx.
    pub max_l2_blocks_behind: NonZeroU32,
    /// Starting fee multiplier when fully up-to-date.
    pub start_max_fee_multiplier: f64,
    /// Ending fee multiplier when nearly max lag.
    pub end_max_fee_multiplier: f64,
    /// Fee that is always acceptable.
    pub always_acceptable_fee: u64,
}

/// Bundling configuration for fuel block submission to L1.
///
/// This configuration controls how blocks are accumulated and bundled.
#[derive(Debug, Clone, Deserialize)]
pub struct BundleConfig {
    /// Time to wait for additional blocks before starting bundling.
    ///
    /// This timeout starts from the last time a bundle was created or from app startup.
    /// Bundling will occur when this timeout expires, even if byte or block thresholds aren’t met.
    #[serde(deserialize_with = "human_readable_duration")]
    pub accumulation_timeout: Duration,

    /// Byte threshold to trigger bundling immediately.
    ///
    /// If this many bytes are accumulated before the timeout, bundling starts right away.
    #[serde(deserialize_with = "human_readable_bytes")]
    pub bytes_to_accumulate: NonZeroUsize,

    /// Block count threshold to trigger bundling if enough unbundled blocks are present.
    pub blocks_to_accumulate: NonZeroUsize,

    /// Maximum number of fragments per bundle. Limits the size of the bundle.
    pub max_fragments_per_bundle: NonZeroUsize,

    /// Maximum time to search for the optimal bundle size.
    ///
    /// When this duration expires, bundling proceeds with the best size found.
    #[serde(deserialize_with = "human_readable_duration")]
    pub optimization_timeout: Duration,

    /// Initial step size for the optimization search.
    ///
    /// For example, with an optimization step of 100 on 1000 blocks, the attempts would be:
    /// 1000, 900, …, 100, 1, 950, 850, …, 50, 975, 925, …
    pub optimization_step: NonZeroUsize,

    /// Timeout to wait for additional fragments before submitting to L1.
    ///
    /// Starts from the last submitted fragment; if no new ones arrive, the accumulated fragments are sent.
    #[serde(deserialize_with = "human_readable_duration")]
    pub fragment_accumulation_timeout: Duration,

    /// Number of fragments to accumulate before submission.
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

    /// Compression level for block data before submission.
    ///
    /// Options:
    /// - `"disabled"`: No compression.
    /// - `"min"` to `"max"`: Increasingly aggressive compression.
    pub compression_level: CompressionLevel,

    /// Interval to check if a new bundle can be created.
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

fn maybe_human_readable_duration<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let duration_str: Option<String> = Deserialize::deserialize(deserializer)?;
    match duration_str {
        Some(s) => humantime::parse_duration(&s).map(Some).map_err(|e| {
            let msg = format!("Failed to parse duration '{s}': {e}");
            serde::de::Error::custom(msg)
        }),
        None => Ok(None),
    }
}

fn human_readable_bytes<'de, D>(deserializer: D) -> Result<NonZeroUsize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let duration_str: String = Deserialize::deserialize(deserializer).unwrap();

    let num_bytes = Byte::from_str(&duration_str)
        .map_err(|e| {
            let msg = format!("Failed to parse bytes '{duration_str}': {e};");
            serde::de::Error::custom(msg)
        })?
        .as_u64() as usize;

    if num_bytes == 0 {
        return Err(serde::de::Error::custom("num bytes must be greater than 0"));
    }

    Ok(NonZeroUsize::new(num_bytes).expect("just checked"))
}

#[derive(Debug, Clone)]
pub struct Internal {
    pub fuel_errors_before_unhealthy: usize,
    pub eth_errors_before_unhealthy: usize,
    pub balance_update_interval: Duration,
    pub cost_request_limit: usize,
    pub l1_blocks_cached_for_fee_metrics_tracker: usize,
    pub import_batches: ImportBatches,
}

/// Manages batching of incoming fuel blocks before importing them into the database, optimizing memory usage
/// and reducing the number of generated logs.
#[derive(Debug, Clone, Copy)]
pub struct ImportBatches {
    pub max_blocks: usize,
    pub max_cumulative_size: usize,
}

impl Default for ImportBatches {
    fn default() -> Self {
        Self {
            max_blocks: 4000,
            max_cumulative_size: 20_000_000,
        }
    }
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
            import_batches: ImportBatches::default(),
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
        help = "Path to the configuration file (currently unused, all configuration done via ENV)."
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

#[cfg(test)]
mod tests {
    use super::KeySource;

    #[test]
    fn can_deserialize_private_key() {
        // given
        let val = r#""Private(0x1234)""#;

        // when
        let key: KeySource = serde_json::from_str(val).unwrap();

        // then
        assert_eq!(key, KeySource::Private("0x1234".to_owned()));
    }

    #[test]
    fn can_deserialize_kms_key() {
        // given
        let val = r#""Kms(0x1234)""#;

        // when
        let key: KeySource = serde_json::from_str(val).unwrap();

        // then
        assert_eq!(key, KeySource::Kms("0x1234".to_owned()));
    }
}
