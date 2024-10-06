mod state_contract;
use std::time::Duration;

use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    providers::{Provider, ProviderBuilder, WsConnect},
    rpc::types::TransactionRequest,
    signers::local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner},
};
use eth::Address;
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

    pub async fn deploy_state_contract(
        &self,
        kms_key: KmsKey,
        contract_args: ContractArgs,
        tx_max_fee: u128,
        request_timeout: Duration,
    ) -> anyhow::Result<DeployedContract> {
        let prepared_transactions =
            CreateTransactions::prepare(self.ws_url(), &kms_key, contract_args).await?;

        let proxy_contract_address = prepared_transactions.proxy_contract_address()?;

        prepared_transactions
            .deploy(self.ws_url(), &kms_key)
            .await?;

        DeployedContract::connect(
            self.ws_url(),
            proxy_contract_address,
            kms_key,
            tx_max_fee,
            request_timeout,
        )
        .await
    }

    fn wallet(&self, index: u32) -> PrivateKeySigner {
        MnemonicBuilder::<English>::default()
            .phrase(self.mnemonic.as_str())
            .index(index)
            .expect("Should generate a valid derivation path")
            .build()
            .expect("phrase to be correct")
    }

    pub fn ws_url(&self) -> Url {
        format!("ws://localhost:{}", self.port)
            .parse()
            .expect("URL to be well formed")
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
