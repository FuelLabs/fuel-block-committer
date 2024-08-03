const FOUNDRY_PROJECT: &str = concat!(env!("OUT_DIR"), "/foundry");

mod deployed_contract;
pub use deployed_contract::*;

use std::time::Duration;

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
use serde::Deserialize;
use url::Url;

use crate::kms::KmsKey;

#[derive(Debug, Clone, Copy)]
pub struct ContractArgs {
    pub finalize_duration: Duration,
    pub blocks_per_interval: u32,
    pub cooldown_between_commits: Duration,
}

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

struct CreateTransactions {
    txs: Vec<CreateTransaction>,
}

impl CreateTransactions {
    async fn deploy(self, middleware: impl Middleware + 'static) -> anyhow::Result<()> {
        for tx in self.txs {
            let status = middleware
                .send_transaction(tx.tx, None)
                .await?
                .confirmations(1)
                .interval(Duration::from_millis(100))
                .await?
                .ok_or_else(|| anyhow::anyhow!("No receipts"))?
                .status
                .ok_or_else(|| anyhow::anyhow!("No status"))?;

            if status != 1.into() {
                anyhow::bail!("Failed to deploy contract {}", tx.name);
            }
        }

        Ok(())
    }

    fn proxy_contract_address(&self) -> anyhow::Result<Address> {
        self.txs
            .iter()
            .find_map(|tx| {
                if tx.name == "ERC1967Proxy" {
                    Some(tx.address)
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                anyhow::anyhow!("No proxy contract address found in prepared transactions")
            })
    }

    fn new(transactions: Vec<CreateTransaction>) -> Self {
        Self { txs: transactions }
    }
}

struct CreateTransaction {
    name: String,
    address: Address,
    tx: Eip1559TransactionRequest,
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

    pub async fn deploy_state_contract(
        &self,
        kms_key: KmsKey,
        contract_args: ContractArgs,
    ) -> anyhow::Result<DeployedContract> {
        let prepared_transactions = self
            .prepare_transactions(
                &kms_key,
                contract_args.finalize_duration.as_secs(),
                contract_args.blocks_per_interval,
                contract_args.cooldown_between_commits.as_secs() as u32,
            )
            .await?;

        let proxy_contract_address = prepared_transactions.proxy_contract_address()?;

        let provider = self.provider(&kms_key).await?;
        prepared_transactions.deploy(provider).await?;

        DeployedContract::connect(&self.ws_url(), proxy_contract_address, kms_key).await
    }

    async fn provider(
        &self,
        kms_key: &KmsKey,
    ) -> Result<SignerMiddleware<Provider<Ws>, ethers::signers::AwsSigner>, anyhow::Error> {
        let provider = Provider::<Ws>::connect(self.ws_url()).await?;
        let signer = kms_key.signer.clone();
        let signer_middleware = SignerMiddleware::new(provider, signer);
        Ok(signer_middleware)
    }

    async fn prepare_transactions(
        &self,
        kms_key: &KmsKey,
        seconds_to_finalize: u64,
        blocks_per_commit_interval: u32,
        commit_cooldown_seconds: u32,
    ) -> Result<CreateTransactions, anyhow::Error> {
        let stdout = self
            .run_deploy_script(
                kms_key,
                seconds_to_finalize,
                blocks_per_commit_interval,
                commit_cooldown_seconds,
            )
            .await?;

        let transactions_file = extract_transactions_file_path(stdout)?;

        let contents = tokio::fs::read_to_string(&transactions_file).await?;
        let broadcasts: Broadcasts = serde_json::from_str(&contents)?;

        let transactions = broadcasts
            .transactions
            .into_iter()
            .map(|tx| CreateTransaction {
                name: tx.name,
                address: tx.address,
                tx: Eip1559TransactionRequest {
                    from: Some(tx.raw_tx.from),
                    gas: Some(tx.raw_tx.gas),
                    value: Some(tx.raw_tx.value),
                    data: Some(tx.raw_tx.input),
                    chain_id: Some(tx.raw_tx.chain_id),
                    ..Default::default()
                },
            })
            .collect::<Vec<_>>();

        Ok(CreateTransactions::new(transactions))
    }

    async fn run_deploy_script(
        &self,
        kms_key: &KmsKey,
        seconds_to_finalize: u64,
        blocks_per_commit_interval: u32,
        commit_cooldown_seconds: u32,
    ) -> Result<String, anyhow::Error> {
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

        Ok(String::from_utf8(output.stdout)?)
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

fn extract_transactions_file_path(stdout: String) -> Result<String, anyhow::Error> {
    let match_txt = "Transactions saved to: ";
    let transactions_file = stdout
        .lines()
        .find(|line| line.contains(match_txt))
        .ok_or_else(|| anyhow::anyhow!("no line in output contains text {match_txt}"))?
        .replace(match_txt, "")
        .trim()
        .to_string();
    Ok(transactions_file)
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
