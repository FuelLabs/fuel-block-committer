use std::{path::Path, time::Duration};

use anyhow::Context;
use services::types::{Address, BundleCost};
use url::Url;

#[derive(Default)]
pub struct Committer {
    show_logs: bool,
    main_key_arn: Option<String>,
    blob_key_arn: Option<String>,
    alt_da_key_arn: Option<String>,
    state_contract_address: Option<String>,
    eth_rpc: Option<Url>,
    fuel_rpc: Option<Url>,
    db_port: Option<u16>,
    db_name: Option<String>,
    kms_url: Option<String>,
    bundle_accumulation_timeout: Option<String>,
    block_bytes_to_accumulate: Option<String>,
    bundle_optimization_step: Option<String>,
    bundle_optimization_timeout: Option<String>,
    bundle_block_height_lookback: Option<String>,
    bundle_compression_level: Option<String>,
    bundle_fragments_to_accumulate: Option<String>,
    bundle_fragment_accumulation_timeout: Option<String>,
    new_bundle_check_interval: Option<String>,
    state_pruner_retention: Option<String>,
    state_pruner_run_interval: Option<String>,
}

impl Committer {
    pub async fn start(self) -> anyhow::Result<CommitterProcess> {
        macro_rules! get_field {
            ($field:ident) => {
                self.$field
                    .ok_or_else(|| anyhow::anyhow!(concat!(stringify!($field), " not provided")))?
            };
        }
        let unused_port = portpicker::pick_unused_port()
            .ok_or_else(|| anyhow::anyhow!("No free port to start fuel-block-committer"))?;

        let kms_url = get_field!(kms_url);
        let mut cmd = tokio::process::Command::new("fuel-block-committer");

        let db_port = get_field!(db_port);
        let db_name = get_field!(db_name);

        let main_key = format!("Kms({})", get_field!(main_key_arn));
        let blob_key = self.blob_key_arn.map(|k| format!("Kms({k})"));
        cmd.env("E2E_TEST_AWS_ENDPOINT", kms_url)
            .env("AWS_REGION", "us-east-1")
            .env("AWS_ACCESS_KEY_ID", "test")
            .env("AWS_SECRET_ACCESS_KEY", "test")
            .env("COMMITTER__ETH__L1_KEYS__MAIN", main_key)
            .env("COMMITTER__ETH__RPC", get_field!(eth_rpc).as_str())
            .env(
                "COMMITTER__ETH__STATE_CONTRACT_ADDRESS",
                get_field!(state_contract_address),
            )
            .env(
                "COMMITTER__FUEL__GRAPHQL_ENDPOINT",
                get_field!(fuel_rpc).as_str(),
            )
            .env("COMMITTER__FUEL__NUM_BUFFERED_REQUESTS", "5")
            .env("COMMITTER__APP__DB__PORT", db_port.to_string())
            .env("COMMITTER__APP__DB__HOST", "localhost")
            .env("COMMITTER__APP__DB__USERNAME", "username")
            .env("COMMITTER__APP__DB__PASSWORD", "password")
            .env("COMMITTER__APP__DB__MAX_CONNECTIONS", "10")
            .env("COMMITTER__APP__DB__USE_SSL", "false")
            .env("COMMITTER__APP__DB__DATABASE", &db_name)
            .env("COMMITTER__APP__PORT", unused_port.to_string())
            .env("COMMITTER__APP__HOST", "127.0.0.1")
            .env("COMMITTER__APP__BLOCK_CHECK_INTERVAL", "5s")
            .env("COMMITTER__APP__TX_FINALIZATION_CHECK_INTERVAL", "5s")
            .env("COMMITTER__APP__L1_FEE_CHECK_INTERVAL", "5s")
            .env("COMMITTER__APP__NUM_BLOCKS_TO_FINALIZE_TX", "3")
            .env("COMMITTER__APP__GAS_BUMP_TIMEOUT", "300s")
            .env("COMMITTER__APP__SEND_TX_REQUEST_TIMEOUT", "10s")
            .env(
                "COMMITTER__APP__BUNDLE__ACCUMULATION_TIMEOUT",
                get_field!(bundle_accumulation_timeout),
            )
            .env(
                "COMMITTER__APP__BUNDLE__BYTES_TO_ACCUMULATE",
                get_field!(block_bytes_to_accumulate),
            )
            .env("COMMITTER__APP__BUNDLE__BLOCKS_TO_ACCUMULATE", "3600")
            .env("COMMITTER__APP__BUNDLE__MAX_FRAGMENTS_PER_BUNDLE", "12")
            .env(
                "COMMITTER__APP__BUNDLE__OPTIMIZATION_TIMEOUT",
                get_field!(bundle_optimization_timeout),
            )
            .env(
                "COMMITTER__APP__BUNDLE__BLOCK_HEIGHT_LOOKBACK",
                get_field!(bundle_block_height_lookback),
            )
            .env(
                "COMMITTER__APP__BUNDLE__COMPRESSION_LEVEL",
                get_field!(bundle_compression_level),
            )
            .env(
                "COMMITTER__APP__BUNDLE__OPTIMIZATION_STEP",
                get_field!(bundle_optimization_step),
            )
            .env(
                "COMMITTER__APP__BUNDLE__FRAGMENTS_TO_ACCUMULATE",
                get_field!(bundle_fragments_to_accumulate),
            )
            .env(
                "COMMITTER__APP__BUNDLE__FRAGMENT_ACCUMULATION_TIMEOUT",
                get_field!(bundle_fragment_accumulation_timeout),
            )
            .env(
                "COMMITTER__APP__BUNDLE__NEW_BUNDLE_CHECK_INTERVAL",
                get_field!(new_bundle_check_interval),
            )
            .env(
                "COMMITTER__APP__STATE_PRUNER_RETENTION",
                get_field!(state_pruner_retention),
            )
            .env(
                "COMMITTER__APP__STATE_PRUNER_RUN_INTERVAL",
                get_field!(state_pruner_run_interval),
            )
            .env("COMMITTER__APP__FEE_ALGO__SHORT_SMA_BLOCKS", "1")
            .env("COMMITTER__APP__FEE_ALGO__LONG_SMA_BLOCKS", "1")
            .env("COMMITTER__APP__FEE_ALGO__MAX_L2_BLOCKS_BEHIND", "1")
            .env("COMMITTER__APP__FEE_ALGO__START_MAX_FEE_MULTIPLIER", "1.0")
            .env("COMMITTER__APP__FEE_ALGO__END_MAX_FEE_MULTIPLIER", "1.0")
            // we're basically disabling the fee algo here
            .env(
                "COMMITTER__APP__FEE_ALGO__ALWAYS_ACCEPTABLE_FEE",
                u64::MAX.to_string(),
            )
            .env("COMMITTER__APP__TX_FEES__MAX", "4000000000000000")
            .env("COMMITTER__APP__TX_FEES__MIN_REWARD_PERC", "20")
            .env("COMMITTER__APP__TX_FEES__MAX_REWARD_PERC", "30")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
            .kill_on_drop(true);

        if let Some(key) = blob_key {
            cmd.env("COMMITTER__ETH__L1_KEYS__BLOB", key.clone());
        } else if let Some(key) = self.alt_da_key_arn {
            let key = format!("Kms({key})");

            cmd.env("COMMITTER__DA_LAYER__TYPE", "EigenDA")
             .env(
                 "COMMITTER__DA_LAYER__RPC",
                 "https://disperser-holesky.eigenda.xyz",
             )
             .env(
                 "COMMITTER__DA_LAYER__KEY",
                 key,
            );
        }

        let sink = if self.show_logs {
            std::process::Stdio::inherit
        } else {
            std::process::Stdio::null
        };
        cmd.stdout(sink()).stderr(sink());

        let child = cmd.spawn().with_context(||"couldn't find `fuel-block-committer` on PATH. Either use `run_tests.sh` or place the binary on the PATH.")?;

        Ok(CommitterProcess {
            _child: child,
            port: unused_port,
        })
    }

    pub fn with_bundle_fragment_accumulation_timeout(mut self, timeout: String) -> Self {
        self.bundle_fragment_accumulation_timeout = Some(timeout);
        self
    }

    pub fn with_bundle_fragments_to_accumulate(mut self, fragments: String) -> Self {
        self.bundle_fragments_to_accumulate = Some(fragments);
        self
    }

    pub fn with_bundle_optimization_step(mut self, step: String) -> Self {
        self.bundle_optimization_step = Some(step);
        self
    }

    pub fn with_bundle_accumulation_timeout(mut self, timeout: String) -> Self {
        self.bundle_accumulation_timeout = Some(timeout);
        self
    }

    pub fn with_block_bytes_to_accumulate(mut self, blocks: String) -> Self {
        self.block_bytes_to_accumulate = Some(blocks);
        self
    }

    pub fn with_bundle_optimization_timeout(mut self, timeout: String) -> Self {
        self.bundle_optimization_timeout = Some(timeout);
        self
    }

    pub fn with_bundle_block_height_lookback(mut self, lookback: String) -> Self {
        self.bundle_block_height_lookback = Some(lookback);
        self
    }

    pub fn with_new_bundle_check_interval(mut self, timeout: String) -> Self {
        self.new_bundle_check_interval = Some(timeout);
        self
    }

    pub fn with_state_pruner_retention(mut self, timeout: String) -> Self {
        self.state_pruner_retention = Some(timeout);
        self
    }

    pub fn with_state_pruner_run_interval(mut self, timeout: String) -> Self {
        self.state_pruner_run_interval = Some(timeout);
        self
    }

    pub fn with_bundle_compression_level(mut self, level: String) -> Self {
        self.bundle_compression_level = Some(level);
        self
    }

    pub fn with_main_key_arn(mut self, wallet_arn: String) -> Self {
        self.main_key_arn = Some(wallet_arn);
        self
    }

    pub fn with_kms_url(mut self, kms_url: String) -> Self {
        self.kms_url = Some(kms_url);
        self
    }

    pub fn with_blob_key_arn(mut self, blob_wallet_arn: String) -> Self {
        self.blob_key_arn = Some(blob_wallet_arn);
        self
    }

    pub fn with_alt_da_key_arn(mut self, alt_da_key_arn: String) -> Self {
        self.alt_da_key_arn = Some(alt_da_key_arn);
        self
    }

    pub fn with_state_contract_address(mut self, state_contract_address: Address) -> Self {
        self.state_contract_address = Some(hex::encode(state_contract_address));
        self
    }

    pub fn with_eth_rpc(mut self, eth_rpc: Url) -> Self {
        self.eth_rpc = Some(eth_rpc);
        self
    }

    pub fn with_fuel_rpc(mut self, fuel_rpc: Url) -> Self {
        self.fuel_rpc = Some(fuel_rpc);
        self
    }

    pub fn with_db_port(mut self, db_port: u16) -> Self {
        self.db_port = Some(db_port);
        self
    }

    pub fn with_db_name(mut self, db_name: String) -> Self {
        self.db_name = Some(db_name);
        self
    }

    pub fn with_show_logs(mut self, show_logs: bool) -> Self {
        self.show_logs = show_logs;
        self
    }
}

pub struct CommitterProcess {
    _child: tokio::process::Child,
    port: u16,
}

impl CommitterProcess {
    pub async fn wait_for_committed_block(&self, height: u64) -> anyhow::Result<()> {
        loop {
            match self.fetch_latest_committed_block().await {
                Ok(current_height) if current_height >= height => break,
                _ => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            }
        }
        Ok(())
    }

    async fn fetch_latest_committed_block(&self) -> anyhow::Result<u64> {
        let metric = self
            .fetch_metric_value("height_of_latest_commitment")
            .await?;
        Ok(metric)
    }

    pub async fn fetch_metric_value(&self, metric_name: &str) -> anyhow::Result<u64> {
        let response = reqwest::get(format!("http://localhost:{}/metrics", self.port))
            .await?
            .error_for_status()?
            .text()
            .await?;

        let height_line = response
            .lines()
            .find(|line| line.starts_with(metric_name))
            .ok_or_else(|| anyhow::anyhow!("couldn't find {} metric", metric_name))?;

        Ok(height_line
            .split_whitespace()
            .last()
            .expect("metric format to be in the format 'NAME VAL'")
            .parse()?)
    }

    pub async fn fetch_costs(
        &self,
        from_height: u32,
        limit: usize,
    ) -> anyhow::Result<Vec<BundleCost>> {
        let response = reqwest::get(format!(
            "http://localhost:{}/v1/costs?variant=specific&value={}&limit={}",
            self.port, from_height, limit
        ))
        .await?
        .error_for_status()?
        .json()
        .await?;

        Ok(response)
    }
}
