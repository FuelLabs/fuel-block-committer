use std::path::PathBuf;

use fuel::HttpClient;
use fuel_core_chain_config::{
    ChainConfig, CoinConfig, ConsensusConfig, SnapshotWriter, StateConfig,
};
use fuel_core_types::{
    fuel_crypto::SecretKey as FuelSecretKey,
    fuel_tx::{AssetId, Finalizable, Input, Output, TransactionBuilder, TxPointer},
    fuel_types::Address,
};
use futures::{stream, StreamExt};
use itertools::Itertools;
use rand::Rng;
use url::Url;

#[derive(Default, Debug)]
pub struct FuelNode {
    show_logs: bool,
}

pub struct FuelNodeProcess {
    _db_dir: tempfile::TempDir,
    _snapshot_dir: tempfile::TempDir,
    _child: tokio::process::Child,
    wallet_keys: Vec<FuelSecretKey>,
    url: Url,
}

impl FuelNode {
    fn create_state_config(
        path: impl Into<PathBuf>,
        num_wallets: usize,
    ) -> anyhow::Result<Vec<FuelSecretKey>> {
        let chain_config = ChainConfig::local_testnet();

        let mut rng = &mut rand::thread_rng();
        let keys = std::iter::repeat_with(|| FuelSecretKey::random(&mut rng))
            .take(num_wallets)
            .collect_vec();

        let coins = keys
            .iter()
            .flat_map(|key| {
                std::iter::repeat_with(|| CoinConfig {
                    owner: Input::owner(&key.public_key()),
                    amount: u64::MAX,
                    asset_id: AssetId::zeroed(),
                    tx_id: rng.gen(),
                    output_index: rng.gen(),
                    ..Default::default()
                })
                .take(10)
                .collect_vec()
            })
            .collect_vec();

        let state_config = StateConfig {
            coins,
            ..StateConfig::local_testnet()
        };

        let snapshot = SnapshotWriter::json(path);
        snapshot
            .write_state_config(state_config, &chain_config)
            .map_err(|_| anyhow::anyhow!("Failed to write state config"))?;

        Ok(keys)
    }

    pub async fn start(&self) -> anyhow::Result<FuelNodeProcess> {
        let db_dir = tempfile::tempdir()?;
        let unused_port = portpicker::pick_unused_port()
            .ok_or_else(|| anyhow::anyhow!("No free port to start fuel-core"))?;

        let mut cmd = tokio::process::Command::new("fuel-core");

        let snapshot_dir = tempfile::tempdir()?;
        let wallet_keys = Self::create_state_config(snapshot_dir.path(), 1000)?;

        // This ensures forward compatibility when running against a newer node with a different native executor version.
        // If the node detects our older version in the chain configuration, it defaults to using the wasm executor.
        // However, since we don't include a wasm executor, this would lead to code loading failure and a node crash.
        // To prevent this, we force the node to use our version number to refer to its native executor.
        let executor_version = fuel_core_types::blockchain::header::LATEST_STATE_TRANSITION_VERSION;

        // The lower limit for 100 Full blocks is somewhere between 400k and 500k
        let gql_complexity = "--graphql-max-complexity=500000";
        cmd.arg("run")
            .arg(gql_complexity)
            .arg("--port")
            .arg(unused_port.to_string())
            .arg("--snapshot")
            .arg(snapshot_dir.path())
            .arg("--db-path")
            .arg(db_dir.path())
            .arg("--debug")
            .arg(format!("--native-executor-version={executor_version}"))
            .arg("--da-compression")
            .arg("1hr")
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null());

        let sink = if self.show_logs {
            std::process::Stdio::inherit
        } else {
            std::process::Stdio::null
        };
        cmd.stdout(sink()).stderr(sink());

        let child = cmd.spawn()?;

        let url = format!("http://localhost:{}", unused_port).parse()?;

        let process = FuelNodeProcess {
            _child: child,
            _db_dir: db_dir,
            url,
            _snapshot_dir: snapshot_dir,
            wallet_keys,
        };

        process.wait_until_healthy().await;

        Ok(process)
    }

    pub fn with_show_logs(mut self, show_logs: bool) -> Self {
        self.show_logs = show_logs;
        self
    }
}

impl FuelNodeProcess {
    pub fn client(&self) -> HttpClient {
        HttpClient::new(&self.url, 5, 5.try_into().unwrap())
    }

    pub async fn produce_transactions(&self, amount: usize) -> anyhow::Result<()> {
        let num_wallets = self.wallet_keys.len();

        let keys = self
            .wallet_keys
            .iter()
            .cloned()
            .cycle()
            .take(amount)
            .collect_vec();

        stream::iter(keys)
            .map(|key| async move { Self::send_transfer_tx(self.client(), key).await })
            .buffered(num_wallets)
            .for_each(|_| async {})
            .await;

        Ok(())
    }

    async fn send_transfer_tx(client: HttpClient, key: FuelSecretKey) -> anyhow::Result<()> {
        let mut tx = TransactionBuilder::script(vec![], vec![]);

        tx.script_gas_limit(1_000_000);

        let secret_key = key;
        let address = Input::owner(&secret_key.public_key());

        let base_asset = AssetId::zeroed();
        let coin = client.get_coin(address, base_asset).await?;

        tx.add_unsigned_coin_input(
            secret_key,
            coin.utxo_id,
            coin.amount,
            coin.asset_id,
            TxPointer::default(),
        );

        const AMOUNT: u64 = 1;
        let to = Address::default();
        tx.add_output(Output::Coin {
            to,
            amount: AMOUNT,
            asset_id: base_asset,
        });
        tx.add_output(Output::Change {
            to: address,
            amount: 0,
            asset_id: base_asset,
        });

        let tx = tx.finalize();

        client.send_tx(&tx.into()).await?;

        Ok(())
    }

    async fn wait_until_healthy(&self) {
        loop {
            if let Ok(true) = self.client().health().await {
                break;
            }
        }
    }

    pub fn url(&self) -> &Url {
        &self.url
    }
}
