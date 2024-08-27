mod state_contract;
use std::{future, time::Duration};

use alloy::{
    consensus::constants::EIP4844_TX_TYPE_ID,
    network::{EthereumWallet, TransactionBuilder},
    providers::{Provider, ProviderBuilder, WsConnect},
    rpc::types::TransactionRequest,
    signers::{
        local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner},
        Signer,
    },
};
use alloy_chains::NamedChain;
use eth::Address;
use futures_util::StreamExt;
use ports::types::U256;
use state_contract::CreateTransactions;
pub use state_contract::{ContractArgs, DeployedContract};
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

        let mnemonic =
            alloy::signers::local::coins_bip39::Mnemonic::<English>::new(&mut rand::thread_rng())
                .to_phrase();

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
            NamedChain::AnvilHardhat.into(),
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

    pub async fn deploy_state_contract(
        &self,
        kms_key: KmsKey,
        contract_args: ContractArgs,
    ) -> anyhow::Result<DeployedContract> {
        let prepared_transactions =
            CreateTransactions::prepare(self.ws_url(), &kms_key, contract_args).await?;

        let proxy_contract_address = prepared_transactions.proxy_contract_address()?;

        prepared_transactions
            .deploy(self.ws_url(), &kms_key)
            .await?;

        DeployedContract::connect(self.ws_url(), proxy_contract_address, kms_key).await
    }

    fn wallet(&self, index: u32) -> PrivateKeySigner {
        MnemonicBuilder::<English>::default()
            .phrase(self.mnemonic.as_str())
            .index(index)
            .expect("Should generate a valid derivation path")
            .build()
            .expect("phrase to be correct")
            .with_chain_id(Some(self.chain_id))
    }

    pub fn ws_url(&self) -> Url {
        format!("ws://localhost:{}", self.port)
            .parse()
            .expect("URL to be well formed")
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub async fn wait_for_included_blob(&self) -> anyhow::Result<()> {
        let ws = WsConnect::new(self.ws_url());
        let provider = ProviderBuilder::new().on_ws(ws).await?;

        let subscription = provider.subscribe_blocks().await?;
        subscription
            .into_stream()
            .take_while(|block| {
                future::ready({
                    block.transactions.txns().any(|tx| {
                        tx.transaction_type
                            .map(|tx_type| tx_type == EIP4844_TX_TYPE_ID)
                            .unwrap_or(false)
                    })
                })
            })
            .collect::<Vec<_>>()
            .await;

        Ok(())
    }

    pub async fn fund(&self, address: Address, amount: U256) -> anyhow::Result<()> {
        let wallet = EthereumWallet::from(self.wallet(0));
        let ws = WsConnect::new(self.ws_url());
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .wallet(wallet)
            .on_ws(ws)
            .await?;

        let tx = TransactionRequest::default()
            .with_to(address)
            .with_value(amount);

        let succeeded = provider
            .send_transaction(tx)
            .await?
            .with_required_confirmations(1)
            .with_timeout(Some(Duration::from_secs(1)))
            .get_receipt()
            .await?
            .status();

        if succeeded {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Failed to fund address {address}"))
        }
    }
}
