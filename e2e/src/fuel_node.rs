use std::{path::PathBuf, str::FromStr};

use fuel::HttpClient;
use fuel_core_chain_config::{
    ChainConfig, ConsensusConfig, SnapshotWriter, StateConfig, TESTNET_WALLET_SECRETS,
};
use fuel_core_types::{
    fuel_crypto::SecretKey as FuelSecretKey,
    fuel_tx::{AssetId, Finalizable, Input, Output, TransactionBuilder, TxPointer},
    fuel_types::Address,
    fuel_vm::SecretKey as FuelKey,
};
use ports::fuel::FuelPublicKey;
use url::Url;

#[derive(Default, Debug)]
pub struct FuelNode {
    show_logs: bool,
}

pub struct FuelNodeProcess {
    _db_dir: tempfile::TempDir,
    _child: tokio::process::Child,
    url: Url,
    public_key: FuelPublicKey,
}

impl FuelNode {
    fn create_state_config(
        path: impl Into<PathBuf>,
        consensus_key: &FuelPublicKey,
    ) -> anyhow::Result<()> {
        let chain_config = ChainConfig {
            consensus: ConsensusConfig::PoA {
                signing_key: Input::owner(consensus_key),
            },
            ..ChainConfig::local_testnet()
        };
        let state_config = StateConfig::local_testnet();

        let snapshot = SnapshotWriter::json(path);
        snapshot
            .write_state_config(state_config, &chain_config)
            .map_err(|_| anyhow::anyhow!("Failed to write state config"))?;

        Ok(())
    }

    pub async fn start(&self) -> anyhow::Result<FuelNodeProcess> {
        let db_dir = tempfile::tempdir()?;
        let unused_port = portpicker::pick_unused_port()
            .ok_or_else(|| anyhow::anyhow!("No free port to start fuel-core"))?;

        let mut cmd = tokio::process::Command::new("fuel-core");

        let secret_key = FuelSecretKey::random(&mut rand::thread_rng());
        let public_key = secret_key.public_key();

        let snapshot_dir = tempfile::tempdir()?;
        Self::create_state_config(snapshot_dir.path(), &public_key)?;

        cmd.arg("run")
            .arg("--port")
            .arg(unused_port.to_string())
            .arg("--snapshot")
            .arg(snapshot_dir.path())
            .arg("--db-path")
            .arg(db_dir.path())
            .arg("--debug")
            .env(
                "CONSENSUS_KEY_SECRET",
                format!("{}", secret_key.to_string()),
            )
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
            public_key,
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
        HttpClient::new(&self.url, 5)
    }

    pub async fn produce_transaction(&self) -> anyhow::Result<()> {
        let mut tx = TransactionBuilder::script(vec![], vec![]);

        tx.script_gas_limit(1_000_000);

        let secret = TESTNET_WALLET_SECRETS[0];
        let secret_key = FuelKey::from_str(&secret).expect("valid secret key");
        let address = Input::owner(&secret_key.public_key());

        let base_asset = AssetId::zeroed();
        let coin = self.client().get_coin(address, base_asset).await?;

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
        self.client().send_tx(&tx.into()).await?;

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

    pub fn consensus_pub_key(&self) -> FuelPublicKey {
        self.public_key
    }
}
