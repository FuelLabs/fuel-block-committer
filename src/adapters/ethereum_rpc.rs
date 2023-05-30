use std::{str::FromStr, sync::Arc};

use async_trait::async_trait;
use ethers::{
    prelude::{abigen, ContractError, SignerMiddleware},
    providers::{Http, Middleware, Provider},
    signers::{LocalWallet, Signer},
    types::{Address, Chain, TransactionReceipt, H256, U256},
};
use fuels::{accounts::fuel_crypto::fuel_types::Bytes20, types::block::Block};
use tracing::log::warn;
use url::Url;

use crate::{
    common::EthTxStatus,
    errors::{Error, Result},
    telemetry::{ConnectionHealthTracker, HealthChecker, RegistersMetrics},
};

use super::eth_metrics::EthMetrics;

#[async_trait]
pub trait EthereumAdapter: Send + Sync {
    async fn submit(&self, block: Block) -> Result<H256>;
    async fn poll_tx_status(&self, tx_hash: H256) -> Result<EthTxStatus>;
}

abigen!(
    FUEL_STATE_CONTRACT,
    r#"[
        function commit(bytes32 blockHash, uint256 commitHeight) external whenNotPaused
    ]"#,
);

#[derive(Clone)]
pub struct EthereumRPC {
    provider: Provider<Http>,
    contract: FUEL_STATE_CONTRACT<SignerMiddleware<Provider<Http>, LocalWallet>>,
    wallet_address: Address,
    metrics: EthMetrics, // TODO: verify Clone leads to the same metric
    health_tracker: ConnectionHealthTracker,
}

impl EthereumRPC {
    pub fn new(ethereum_rpc: &Url, contract_address: Bytes20, ethereum_wallet_key: &str) -> Self {
        let provider = Provider::<Http>::try_from(ethereum_rpc.to_string()).unwrap();
        let wallet = LocalWallet::from_str(ethereum_wallet_key)
            .unwrap()
            .with_chain_id(Chain::AnvilHardhat);
        let wallet_address = wallet.address();
        let signer = SignerMiddleware::new(provider.clone(), wallet);

        let contract_address = Address::from_slice(contract_address.as_ref());
        let contract = FUEL_STATE_CONTRACT::new(contract_address, Arc::new(signer));

        Self {
            provider,
            contract,
            wallet_address,
            metrics: EthMetrics::default(),
            health_tracker: ConnectionHealthTracker::new(3),
        }
    }

    pub fn connection_health_checker(&self) -> HealthChecker {
        self.health_tracker.tracker()
    }

    fn handle_network_error(&self) {
        self.health_tracker.note_failure();
        self.metrics.eth_network_errors.inc();
    }

    fn handle_network_success(&self) {
        self.health_tracker.note_success();
    }

    fn extract_status(receipt: Option<TransactionReceipt>) -> EthTxStatus {
        let Some(receipt) = receipt else {
            return EthTxStatus::Pending;
        };

        let status = receipt
            .status
            .expect("Status field should be present after EIP-658!");

        if status.is_zero() {
            return EthTxStatus::Aborted;
        } else {
            return EthTxStatus::Commited;
        }
    }

    async fn record_balance(&self) -> Result<()> {
        let balance = self
            .provider
            .get_balance(self.wallet_address, None)
            .await
            .map_err(|err| {
                self.handle_network_error();
                Error::NetworkError(err.to_string())
            })?;
        self.handle_network_success();

        warn!("{}", &balance);

        // Note: might lead to wrong metrics if we have more than 500k ETH
        let balance_gwei = balance / U256::from(1_000_000_000);
        self.metrics.eth_wallet_balance.set(balance_gwei.as_u64() as i64);

        Ok(())
    }
}

impl RegistersMetrics for EthereumRPC {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        self.metrics.metrics()
    }
}

#[async_trait]
impl EthereumAdapter for EthereumRPC {
    async fn submit(&self, block: Block) -> Result<H256> {
        let contract_call = self.contract.commit(*block.id, block.header.height.into());
        let tx = contract_call
            .send()
            .await
            .map_err(|contract_err| match contract_err {
                ContractError::ProviderError { e } => {
                    self.handle_network_error();
                    Error::NetworkError(e.to_string())
                }
                _ => Error::Other(contract_err.to_string()),
            });
        self.handle_network_success();

        self.record_balance().await?;

        let id = tx?.tx_hash();
        warn!("{}", &id);

        Ok(id)
    }

    async fn poll_tx_status(&self, tx_hash: H256) -> Result<EthTxStatus> {
        let result = self
            .provider
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(|err| {
                self.handle_network_error();
                Error::NetworkError(err.to_string())
            })?;
        self.handle_network_success();

        Ok(EthereumRPC::extract_status(result))
    }
}
