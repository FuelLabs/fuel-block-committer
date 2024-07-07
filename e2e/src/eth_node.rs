const FOUNDRY_PROJECT: &str = concat!(env!("OUT_DIR"), "/foundry");

use ethers::{
    abi::Address,
    signers::{
        coins_bip39::{English, Mnemonic},
        LocalWallet, MnemonicBuilder, Signer,
    },
    types::Chain,
};
use url::Url;

#[derive(Default, Debug)]
pub struct EthNode {
    show_logs: bool,
}

impl EthNode {
    pub async fn start(&self) -> anyhow::Result<EthNodeProcess> {
        let unused_port = portpicker::pick_unused_port()
            .ok_or_else(|| anyhow::anyhow!("No free port to start anvil"))?;

        let mnemonic = Mnemonic::<English>::new(&mut rand::thread_rng()).to_phrase();

        let mut cmd = tokio::process::Command::new("anvil");

        cmd.arg("--port")
            .arg(unused_port.to_string())
            .arg("--mnemonic")
            .arg(&mnemonic)
            .arg("--block-time")
            .arg("1")
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null());

        let sink = if self.show_logs {
            std::process::Stdio::inherit
        } else {
            std::process::Stdio::null
        };
        cmd.stdout(sink()).stderr(sink());

        let child = cmd.spawn()?;

        Ok(EthNodeProcess::new(child, unused_port, mnemonic))
    }

    pub fn with_show_logs(mut self, show_logs: bool) -> Self {
        self.show_logs = show_logs;
        self
    }
}

pub struct EthNodeProcess {
    _child: tokio::process::Child,
    port: u16,
    mnemonic: String,
}

impl EthNodeProcess {
    fn new(child: tokio::process::Child, port: u16, mnemonic: String) -> Self {
        Self {
            _child: child,
            mnemonic,
            port,
        }
    }

    pub async fn deploy_chain_state_contract(
        &self,
        seconds_to_finalize: u64,
        blocks_per_commit_interval: u32,
        commit_cooldown_seconds: u32,
    ) -> anyhow::Result<Address> {
        let output = tokio::process::Command::new("forge")
            .current_dir(FOUNDRY_PROJECT)
            .arg("script")
            .arg("script/deploy.sol:MyScript")
            .arg("--fork-url")
            .arg(&format!("http://localhost:{}", self.port))
            .arg("--broadcast")
            .stdin(std::process::Stdio::null())
            .env("PRIVATE_KEY", self.wallet_key())
            .env("TIME_TO_FINALIZE", seconds_to_finalize.to_string())
            .env(
                "BLOCKS_PER_COMMIT_INTERVAL",
                blocks_per_commit_interval.to_string(),
            )
            .env("COMMIT_COOLDOWN", commit_cooldown_seconds.to_string())
            .kill_on_drop(true)
            .output()
            .await?;

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "Failed to deploy chain state contract: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        eprintln!("Deployed chain state contract");

        let stdout = String::from_utf8(output.stdout)?;
        let proxy_address = stdout
            .lines()
            .find(|line| line.contains("PROXY:"))
            .ok_or_else(|| anyhow::anyhow!("No proxy address found"))?
            .replace("PROXY:", "")
            .trim()
            .parse()?;

        Ok(proxy_address)
    }

    pub fn wallet_key(&self) -> String {
        let bytes = self.wallet().signer().to_bytes();
        format!("0x{}", hex::encode(bytes))
    }

    pub fn wallet(&self) -> LocalWallet {
        MnemonicBuilder::<English>::default()
            .phrase(self.mnemonic.as_str())
            .build()
            .expect("phrase to be correct")
            .with_chain_id(Chain::AnvilHardhat)
    }

    pub fn ws_url(&self) -> Url {
        format!("ws://localhost:{}", self.port)
            .parse()
            .expect("URL to be well formed")
    }
}
