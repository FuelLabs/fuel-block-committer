const FOUNDRY_PROJECT: &str = concat!(env!("OUT_DIR"), "/foundry");

use std::time::Duration;

use eth::WebsocketClient;
use ethers::middleware::Middleware;
use ethers::providers::{Provider, Ws};
use ethers::types::{Bytes, Eip1559TransactionRequest, U256, U64};
use ethers::{
    abi::Address,
    middleware::SignerMiddleware,
    signers::{
        coins_bip39::{English, Mnemonic},
        LocalWallet, MnemonicBuilder, Signer,
    },
    types::{Chain, TransactionRequest},
};
use ports::types::ValidatedFuelBlock;
use serde::Deserialize;
use url::Url;

use crate::kms::KmsKey;

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

        Ok(EthNodeProcess::new(
            child,
            unused_port,
            Chain::AnvilHardhat.into(),
            mnemonic,
        ))
    }

    pub fn with_show_logs(mut self, show_logs: bool) -> Self {
        self.show_logs = show_logs;
        self
    }
}

pub struct EthNodeProcess {
    _child: tokio::process::Child,
    chain_id: u64,
    port: u16,
    mnemonic: String,
}

impl EthNodeProcess {
    fn new(child: tokio::process::Child, port: u16, chain_id: u64, mnemonic: String) -> Self {
        Self {
            _child: child,
            mnemonic,
            port,
            chain_id,
        }
    }

    pub async fn deploy_contract(
        &self,
        kms_key: KmsKey,
        seconds_to_finalize: u64,
        blocks_per_commit_interval: u32,
        commit_cooldown_seconds: u32,
    ) -> anyhow::Result<DeployedContract> {
        let output = tokio::process::Command::new("forge")
            .current_dir(FOUNDRY_PROJECT)
            .arg("script")
            .arg("script/deploy.sol:MyScript")
            .arg("--fork-url")
            .arg(&format!("http://localhost:{}", self.port()))
            .stdin(std::process::Stdio::null())
            .env("ADDRESS", format!("{:?}", kms_key.address()))
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

        let stdout = String::from_utf8(output.stdout)?;
        let match_txt = "Transactions saved to: ";
        let transactions_file = stdout
            .lines()
            .find(|line| line.contains(match_txt))
            .ok_or_else(|| anyhow::anyhow!("no line in output contains text {match_txt}"))?
            .replace(match_txt, "")
            .trim()
            .to_string();

        let contents = tokio::fs::read_to_string(&transactions_file).await?;
        let broadcasts: Broadcasts = serde_json::from_str(&contents)?;

        let proxy_contract_address = broadcasts
            .transactions
            .iter()
            .find_map(|tx| (tx.name == "ERC1967Proxy").then_some(tx.address))
            .ok_or_else(|| anyhow::anyhow!("No ERC1967Proxy contract created"))?;

        let provider = Provider::<Ws>::connect(self.ws_url()).await?;
        let signer = kms_key.signer.clone();
        let signer_middleware = SignerMiddleware::new(provider, signer);

        for create_tx in broadcasts.transactions {
            let tx = Eip1559TransactionRequest {
                from: Some(create_tx.raw_tx.from),
                gas: Some(create_tx.raw_tx.gas),
                value: Some(create_tx.raw_tx.value),
                data: Some(create_tx.raw_tx.input),
                chain_id: Some(create_tx.raw_tx.chain_id),
                ..Default::default()
            };

            let status = signer_middleware
                .send_transaction(tx, None)
                .await?
                .confirmations(1)
                .interval(Duration::from_millis(100))
                .await?
                .ok_or_else(|| anyhow::anyhow!("No receipts"))?
                .status
                .ok_or_else(|| anyhow::anyhow!("No status"))?;

            if status != 1.into() {
                anyhow::bail!("Failed to deploy contract {}", create_tx.name);
            }
        }

        let deployed_contract = DeployedContract::connect(
            &self.ws_url(),
            proxy_contract_address,
            kms_key,
            seconds_to_finalize,
            blocks_per_commit_interval,
            commit_cooldown_seconds,
        )
        .await?;

        Ok(deployed_contract)
    }

    fn wallet(&self, index: u32) -> LocalWallet {
        MnemonicBuilder::<English>::default()
            .phrase(self.mnemonic.as_str())
            .index(index)
            .expect("Should generate a valid derivation path")
            .build()
            .expect("phrase to be correct")
            .with_chain_id(self.chain_id)
    }

    pub fn ws_url(&self) -> Url {
        format!("ws://localhost:{}", self.port)
            .parse()
            .expect("URL to be well formed")
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub async fn fund(&self, address: Address, amount: U256) -> anyhow::Result<()> {
        let wallet = self.wallet(0);
        let provider = Provider::<Ws>::connect(self.ws_url())
            .await
            .expect("to connect to the provider");

        let signer_middleware = SignerMiddleware::new(provider, wallet);

        let tx = TransactionRequest::pay(address, amount);

        let status = signer_middleware
            .send_transaction(tx, None)
            .await?
            .confirmations(1)
            .interval(Duration::from_millis(100))
            .await?
            .unwrap()
            .status
            .unwrap();

        if status == 1.into() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Failed to fund address {address}"))
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RawTx {
    from: Address,
    gas: U256,
    value: U256,
    input: Bytes,
    #[serde(rename = "chainId")]
    chain_id: U64,
}
#[derive(Debug, Clone, Deserialize)]
struct CreateContractTx {
    #[serde(rename = "contractName")]
    name: String,
    #[serde(rename = "contractAddress")]
    address: Address,
    #[serde(rename = "transaction")]
    raw_tx: RawTx,
}

#[derive(Debug, Clone, Deserialize)]
struct Broadcasts {
    transactions: Vec<CreateContractTx>,
}

pub struct DeployedContract {
    address: Address,
    chain_state_contract: WebsocketClient,
    duration_to_finalize: Duration,
    blocks_per_commit_interval: u32,
    _commit_cooldown_seconds: u32,
}

impl DeployedContract {
    async fn connect(
        url: &Url,
        address: Address,
        key: KmsKey,
        seconds_to_finalize: u64,
        blocks_per_commit_interval: u32,
        commit_cooldown_seconds: u32,
    ) -> anyhow::Result<Self> {
        let blob_wallet = None;
        let region: String = serde_json::to_string(&key.region)?;
        let chain_state_contract = WebsocketClient::connect(
            url,
            Chain::AnvilHardhat,
            address,
            key.id,
            blob_wallet,
            5,
            region,
            "test".to_string(),
            "test".to_string(),
            true,
        )
        .await?;

        Ok(Self {
            address,
            chain_state_contract,
            duration_to_finalize: Duration::from_secs(seconds_to_finalize),
            blocks_per_commit_interval,
            _commit_cooldown_seconds: commit_cooldown_seconds,
        })
    }

    pub async fn finalized(&self, block: ValidatedFuelBlock) -> anyhow::Result<bool> {
        self.chain_state_contract
            .finalized(block)
            .await
            .map_err(Into::into)
    }

    pub fn address(&self) -> Address {
        self.address
    }

    pub fn duration_to_finalize(&self) -> Duration {
        self.duration_to_finalize
    }

    pub fn blocks_per_commit_interval(&self) -> u32 {
        self.blocks_per_commit_interval
    }

    #[allow(dead_code)]
    pub fn commit_cooldown_seconds(&self) -> u32 {
        self._commit_cooldown_seconds
    }
}
