mod state_contract;
use crate::kms::KmsKey;
use ethers::middleware::Middleware;
use ethers::providers::{Provider, Ws};
use ethers::types::U256;
use ethers::{
    abi::Address,
    middleware::SignerMiddleware,
    signers::{
        coins_bip39::{English, Mnemonic},
        LocalWallet, MnemonicBuilder, Signer,
    },
    types::{Chain, TransactionRequest},
};
use state_contract::CreateTransactions;
pub use state_contract::{ContractArgs, DeployedContract};
use std::time::Duration;
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

    pub async fn deploy_state_contract(
        &self,
        kms_key: KmsKey,
        contract_args: ContractArgs,
    ) -> anyhow::Result<DeployedContract> {
        let prepared_transactions =
            CreateTransactions::prepare(self.port(), &kms_key, contract_args).await?;

        let proxy_contract_address = prepared_transactions.proxy_contract_address()?;

        prepared_transactions
            .deploy(self.ws_url(), &kms_key)
            .await?;

        DeployedContract::connect(&self.ws_url(), proxy_contract_address, kms_key).await
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
