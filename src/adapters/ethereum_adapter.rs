use std::{str::FromStr, sync::Arc};

use async_trait::async_trait;
use ethers::{
    prelude::{abigen, ContractError, SignerMiddleware},
    providers::{Middleware, Provider, Ws},
    signers::{LocalWallet, Signer},
    types::{Address, Chain, TransactionReceipt, H256, U256},
};
use fuels::{accounts::fuel_crypto::fuel_types::Bytes20, types::block::Block};
use tracing::info;
use url::Url;

use super::eth_metrics::EthMetrics;
use crate::{
    common::EthTxStatus,
    errors::{Error, Result},
    telemetry::{ConnectionHealthTracker, HealthChecker, RegistersMetrics},
};

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait EthereumAdapter: Send + Sync {
    async fn submit(&self, block: Block) -> Result<H256>;
    async fn poll_tx_status(&self, tx_hash: H256) -> Result<EthTxStatus>;
    // async fn stream_commit_event(&self, from_block: u64) -> Result<()>;
}

abigen!(
    FUEL_STATE_CONTRACT,
    r#"[
        function commit(bytes32 blockHash, uint256 commitHeight) external whenNotPaused
        event CommitSubmitted(uint256 indexed commitHeight, bytes32 blockHash)
    ]"#,
);

#[derive(Clone)]
pub struct EthereumRPC {
    provider: Provider<Ws>,
    contract: FUEL_STATE_CONTRACT<SignerMiddleware<Provider<Ws>, LocalWallet>>,
    wallet_address: Address,
    metrics: EthMetrics,
    health_tracker: ConnectionHealthTracker,
}

impl EthereumRPC {
    pub async fn connect(
        ethereum_rpc: &Url,
        chain_id: Chain,
        contract_address: Bytes20,
        ethereum_wallet_key: &str,
        unhealthy_after_n_errors: usize,
    ) -> Result<Self> {
        let provider = Provider::<Ws>::connect(ethereum_rpc.to_string())
            .await
            .map_err(|e| Error::NetworkError(e.to_string()))?;

        let wallet = LocalWallet::from_str(ethereum_wallet_key)?.with_chain_id(chain_id);
        let wallet_address = wallet.address();

        let signer = SignerMiddleware::new(provider.clone(), wallet);

        let contract_address = Address::from_slice(contract_address.as_ref());
        let contract = FUEL_STATE_CONTRACT::new(contract_address, Arc::new(signer));

        Ok(Self {
            provider,
            contract,
            wallet_address,
            metrics: EthMetrics::default(),
            health_tracker: ConnectionHealthTracker::new(unhealthy_after_n_errors),
        })
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
            EthTxStatus::Aborted
        } else {
            EthTxStatus::Committed
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

        info!("wallet balance: {}", &balance);

        // Note: might lead to wrong metrics if we have more than 500k ETH
        let balance_gwei = balance / U256::from(1_000_000_000);
        self.metrics
            .eth_wallet_balance
            .set(balance_gwei.as_u64() as i64);

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
                ContractError::MiddlewareError { e } => {
                    self.handle_network_error();
                    Error::NetworkError(e.to_string())
                }
                _ => Error::Other(contract_err.to_string()),
            })?;

        self.handle_network_success();

        self.record_balance().await?;

        Ok(tx.tx_hash())
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

    // fn commit_event_stream(&self, from_block: u64) -> Result<()> {
    //     let events = self.contract
    //         .event::<CommitSubmittedFilter>()
    //         .from_block(from_block);
    // }
}

#[cfg(test)]
mod tests {
    use fuels::{
        tx::Bytes32,
        types::block::{Block as FuelBlock, Header as FuelBlockHeader},
    };
    use prometheus::Registry;

    use super::*;

    #[tokio::test]
    async fn eth_rpc_updates_metrics_in_case_of_network_err() {
        // given
        let ethereum_rpc = given_eth_rpc().await;
        let registry = Registry::default();
        ethereum_rpc.register_metrics(&registry);

        // when
        let result = ethereum_rpc.submit(given_a_block(42)).await;

        // then
        assert!(result.is_err());
        let metrics = registry.gather();
        let network_errors_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "eth_network_errors")
            .and_then(|metric| metric.get_metric().get(0))
            .map(|metric| metric.get_counter())
            .unwrap();

        assert_eq!(network_errors_metric.get_value(), 1f64);
    }

    #[tokio::test]
    async fn eth_rpc_clone_updates_shared_metrics() {
        // given
        let ethereum_rpc = given_eth_rpc().await;
        let registry = Registry::default();
        ethereum_rpc.register_metrics(&registry);

        let ethereum_rpc_clone = ethereum_rpc.clone();

        // when
        let _ = ethereum_rpc.submit(given_a_block(42)).await;
        let _ = ethereum_rpc_clone.submit(given_a_block(42)).await;

        // then
        let metrics = registry.gather();
        let network_errors_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "eth_network_errors")
            .and_then(|metric| metric.get_metric().get(0))
            .map(|metric| metric.get_counter())
            .unwrap();

        assert_eq!(network_errors_metric.get_value(), 2f64);
    }

    #[tokio::test]
    async fn eth_rpc_correctly_tracks_network_health() {
        let ethereum_rpc = given_eth_rpc().await;
        let health_check = ethereum_rpc.connection_health_checker();

        assert!(health_check.healthy());

        let _ = ethereum_rpc.submit(given_a_block(42)).await;
        assert!(health_check.healthy());

        let _ = ethereum_rpc.submit(given_a_block(42)).await;
        assert!(health_check.healthy());

        let _ = ethereum_rpc.submit(given_a_block(42)).await;
        assert!(!health_check.healthy());
    }

    async fn given_eth_rpc() -> EthereumRPC {
        let url = Url::parse("http://127.0.0.42:42").unwrap();
        let wallet_key = "0x9e56ccf010fa4073274b8177ccaad46fbaf286645310d03ac9bb6afa922a7c36";
        EthereumRPC::connect(&url, Default::default(), Default::default(), wallet_key, 3)
            .await
            .unwrap()
    }

    fn given_a_block(block_height: u32) -> FuelBlock {
        let header = FuelBlockHeader {
            id: Bytes32::zeroed(),
            da_height: 0,
            transactions_count: 0,
            message_receipt_count: 0,
            transactions_root: Bytes32::zeroed(),
            message_receipt_root: Bytes32::zeroed(),
            height: block_height,
            prev_root: Bytes32::zeroed(),
            time: None,
            application_hash: Bytes32::zeroed(),
        };

        FuelBlock {
            id: Bytes32::default(),
            header,
            transactions: vec![],
        }
    }
}
