use std::sync::Arc;

use eth::WebsocketClient;
use ethers::{
    abi::Address,
    contract::{Contract, ContractFactory},
    middleware::SignerMiddleware,
    providers::{Provider, Ws},
    signers::{
        coins_bip39::{English, Mnemonic},
        LocalWallet, MnemonicBuilder, Signer,
    },
    types::{Chain, U256},
};
use url::Url;

use crate::chain_state_contract::{self};

pub struct EthNode;

impl EthNode {
    pub async fn start() -> anyhow::Result<EthNodeProcess> {
        let unused_port = portpicker::pick_unused_port()
            .ok_or_else(|| anyhow::anyhow!("No free port to start anvil"))?;

        let mnemonic = Mnemonic::<English>::new(&mut rand::thread_rng()).to_phrase();

        let child = tokio::process::Command::new("anvil")
            .arg("--port")
            .arg(unused_port.to_string())
            .arg("--mnemonic")
            .arg(&mnemonic)
            .arg("--block-time")
            .arg("1")
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        Ok(EthNodeProcess::new(child, unused_port, mnemonic))
    }
}

pub struct EthNodeProcess {
    _child: tokio::process::Child,
    url: Url,
    mnemonic: String,
}

impl EthNodeProcess {
    fn new(child: tokio::process::Child, port: u16, mnemonic: String) -> Self {
        Self {
            _child: child,
            url: format!("ws://localhost:{}", port)
                .parse()
                .expect("URL to be well formed"),
            mnemonic,
        }
    }

    pub async fn deploy_chain_state_contract(
        &self,
        seconds_to_finalize: u64,
        blocks_per_commit_interval: u32,
        commit_cooldown_seconds: u32,
    ) -> anyhow::Result<Address> {
        let artifacts = chain_state_contract::compilation_artifacts();

        let wallet = self.wallet();

        let provider = Provider::<Ws>::connect(&self.url).await?;

        let client = SignerMiddleware::new(provider, wallet);

        let factory = ContractFactory::new(
            artifacts.abi.clone(),
            artifacts.bytecode.clone(),
            Arc::new(client),
        );

        let deploy_tx = factory.deploy((
            U256::from(seconds_to_finalize),
            U256::from(blocks_per_commit_interval),
            commit_cooldown_seconds,
        ))?;

        let contract = deploy_tx.send().await?;

        let initialize_tx = contract.method::<(), ()>("initialize", ())?;
        eprintln!("initializing contract");
        initialize_tx.send().await?.await?;
        eprintln!("contract initialized");

        Ok(contract.address())
    }

    pub fn wallet_key(&self) -> String {
        let bytes = self.wallet().signer().to_bytes();
        hex::encode(bytes)
    }

    pub fn wallet(&self) -> LocalWallet {
        MnemonicBuilder::<English>::default()
            .phrase(self.mnemonic.as_str())
            .build()
            .expect("phrase to be correct")
            .with_chain_id(Chain::AnvilHardhat)
    }

    pub fn ws_url(&self) -> &Url {
        &self.url
    }
}
