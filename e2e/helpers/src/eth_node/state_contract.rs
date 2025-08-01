const FOUNDRY_PROJECT: &str = concat!(env!("OUT_DIR"), "/foundry");
use std::time::Duration;

use alloy::{
    network::EthereumWallet,
    primitives::{Bytes, TxKind},
    providers::{Provider, ProviderBuilder, WsConnect},
    rpc::types::TransactionRequest,
};
use eth::{AcceptablePriorityFeePercentages, WebsocketClient};
use fs_extra::dir::{CopyOptions, copy};
use serde::Deserialize;
use services::{block_committer::port::fuel::FuelBlock, types::Address};
use signers::eth::{Signer, kms::TestEthKmsSigner};
use tokio::process::Command;
use url::Url;

pub struct DeployedContract {
    address: Address,
    chain_state_contract: WebsocketClient,
}

impl DeployedContract {
    pub async fn connect(
        url: Url,
        address: Address,
        signers: eth::L1Signers<TestEthKmsSigner, TestEthKmsSigner>,
        tx_max_fee: u128,
        send_tx_request_timeout: Duration,
    ) -> anyhow::Result<Self> {
        let chain_state_contract = WebsocketClient::connect(
            url,
            address,
            signers,
            5,
            eth::TxConfig {
                tx_max_fee,
                send_tx_request_timeout,
                acceptable_priority_fee_percentage: AcceptablePriorityFeePercentages::default(),
            },
        )
        .await?;

        Ok(Self {
            address,
            chain_state_contract,
        })
    }

    pub async fn finalized(&self, block: FuelBlock) -> anyhow::Result<bool> {
        self.chain_state_contract
            .finalized(block.id, block.height)
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
        main_wallet_address: Address,
        contract_args: ContractArgs,
    ) -> Result<Self, anyhow::Error> {
        let transactions =
            generate_transactions_via_foundry(url, main_wallet_address, contract_args)
                .await?
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

    pub async fn deploy(self, url: Url, main_signer: Signer) -> anyhow::Result<()> {
        let wallet = EthereumWallet::from(main_signer);

        let ws = WsConnect::new(url);
        let provider = ProviderBuilder::new().wallet(wallet).connect_ws(ws).await?;

        for tx in self.txs {
            let succeeded = provider
                .send_transaction(tx.tx)
                .await?
                .with_required_confirmations(1)
                .with_timeout(Some(Duration::from_secs(60)))
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

async fn generate_transactions_via_foundry(
    url: Url,
    address: Address,
    contract_args: ContractArgs,
) -> anyhow::Result<Vec<CreateContractTx>> {
    let temp_dir = tempfile::tempdir()?;
    let temp_path = temp_dir.path().to_owned();

    let mut options = CopyOptions::new();
    options.content_only = true;

    let destination = temp_path.clone();
    tokio::task::spawn_blocking(move || {
        copy(FOUNDRY_PROJECT, destination, &options).unwrap();
    })
    .await?;

    let output = Command::new("forge")
        .current_dir(temp_path)
        .arg("script")
        .arg("script/build_tx.sol:MyScript")
        .arg("--fork-url")
        .arg(url.to_string())
        .stdin(std::process::Stdio::null())
        .env("ADDRESS", format!("{:?}", address))
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

    let stdout = String::from_utf8(output.stdout)?;
    let transactions_file = extract_transactions_file_path(stdout)?;
    let contents = tokio::fs::read_to_string(transactions_file).await?;

    let broadcasts: Broadcasts = serde_json::from_str(&contents)?;
    Ok(broadcasts.transactions)
}
