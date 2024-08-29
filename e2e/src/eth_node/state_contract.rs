const FOUNDRY_PROJECT: &str = concat!(env!("OUT_DIR"), "/foundry");
use std::time::Duration;

use alloy::{
    network::EthereumWallet,
    primitives::{Bytes, TxKind},
    providers::{Provider, ProviderBuilder, WsConnect},
    rpc::types::TransactionRequest,
};
use eth::{AwsClient, AwsConfig, WebsocketClient};
use ports::types::{Address, ValidatedFuelBlock};
use serde::Deserialize;
use url::Url;

use crate::kms::KmsKey;

pub struct DeployedContract {
    address: Address,
    chain_state_contract: WebsocketClient,
}

impl DeployedContract {
    pub async fn connect(url: Url, address: Address, key: KmsKey) -> anyhow::Result<Self> {
        let blob_wallet = None;
        let aws_client = AwsClient::new(AwsConfig::Test(key.url)).await;

        let chain_state_contract =
            WebsocketClient::connect(url, address, key.id, blob_wallet, 5, aws_client).await?;

        Ok(Self {
            address,
            chain_state_contract,
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
}

#[derive(Debug, Clone, Copy)]
pub struct ContractArgs {
    pub finalize_duration: Duration,
    pub blocks_per_interval: u32,
    pub cooldown_between_commits: Duration,
}

pub struct CreateTransactions {
    txs: Vec<CreateTransaction>,
}

impl CreateTransactions {
    pub async fn prepare(
        url: Url,
        kms_key: &KmsKey,
        contract_args: ContractArgs,
    ) -> Result<Self, anyhow::Error> {
        let stdout = run_tx_building_script(url, kms_key, contract_args).await?;

        let transactions_file = extract_transactions_file_path(stdout)?;

        let contents = tokio::fs::read_to_string(&transactions_file).await?;
        let broadcasts: Broadcasts = serde_json::from_str(&contents)?;

        let transactions = broadcasts
            .transactions
            .into_iter()
            .map(|tx| CreateTransaction {
                name: tx.name,
                address: tx.address,
                tx: TransactionRequest {
                    from: Some(tx.raw_tx.from),
                    input: tx.raw_tx.input.into(),
                    to: Some(TxKind::Create),
                    ..Default::default()
                },
            })
            .collect::<Vec<_>>();

        Ok(CreateTransactions::new(transactions))
    }

    pub async fn deploy(self, url: Url, kms_key: &KmsKey) -> anyhow::Result<()> {
        let wallet = EthereumWallet::from(kms_key.signer.clone());
        let ws = WsConnect::new(url);
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .wallet(wallet)
            .on_ws(ws)
            .await?;

        for tx in self.txs {
            let succeeded = provider
                .send_transaction(tx.tx)
                .await?
                .with_required_confirmations(1)
                .with_timeout(Some(Duration::from_secs(1)))
                .get_receipt()
                .await?
                .status();

            if !succeeded {
                anyhow::bail!("Failed to deploy contract {}", tx.name);
            }
        }

        Ok(())
    }

    pub fn proxy_contract_address(&self) -> anyhow::Result<Address> {
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
    tx: TransactionRequest,
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
    input: Bytes,
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

async fn run_tx_building_script(
    url: Url,
    kms_key: &KmsKey,
    contract_args: ContractArgs,
) -> Result<String, anyhow::Error> {
    let output = tokio::process::Command::new("forge")
        .current_dir(FOUNDRY_PROJECT)
        .arg("script")
        .arg("script/build_tx.sol:MyScript")
        .arg("--fork-url")
        .arg(url.to_string())
        .stdin(std::process::Stdio::null())
        .env("ADDRESS", format!("{:?}", kms_key.address()))
        .env(
            "TIME_TO_FINALIZE",
            contract_args.finalize_duration.as_secs().to_string(),
        )
        .env(
            "BLOCKS_PER_COMMIT_INTERVAL",
            contract_args.blocks_per_interval.to_string(),
        )
        .env(
            "COMMIT_COOLDOWN",
            contract_args.cooldown_between_commits.as_secs().to_string(),
        )
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
