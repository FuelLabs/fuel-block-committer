use std::{str::FromStr, sync::Arc};

use async_trait::async_trait;
use ethers::{
    prelude::{abigen, ContractError, SignerMiddleware},
    providers::{Middleware, Provider, Ws},
    signers::{LocalWallet, Signer},
    types::{Address, Chain, U256, U64},
};
use fuels::{accounts::fuel_crypto::fuel_types::Bytes20, types::block::Block};
use prometheus::{IntGauge, Opts};
use tracing::info;
use url::Url;

use crate::{
    adapters::ethereum_adapter::{EthereumAdapter, EventStreamer},
    errors::{Error, Result},
    telemetry::RegistersMetrics,
};

use super::event_streamer::EthEventStreamer;

abigen!(
    FUEL_STATE_CONTRACT,
    r#"[
        function commit(bytes32 blockHash, uint256 commitHeight) external whenNotPaused
        event CommitSubmitted(uint256 indexed commitHeight, bytes32 blockHash)
    ]"#,
);

#[derive(Clone)]
pub struct EthereumWs {
    provider: Provider<Ws>,
    contract: FUEL_STATE_CONTRACT<SignerMiddleware<Provider<Ws>, LocalWallet>>,
    wallet_address: Address,
    commit_interval: u32,
    metrics: Metrics,
}

impl EthereumWs {
    pub async fn connect(
        ethereum_rpc: &Url,
        chain_id: Chain,
        contract_address: Bytes20,
        ethereum_wallet_key: &str,
        commit_interval: u32,
    ) -> Result<Self> {
        let provider = Provider::<Ws>::connect(ethereum_rpc.to_string())
            .await
            .map_err(|e| Error::NetworkError(e.to_string()))?;

        let wallet = LocalWallet::from_str(ethereum_wallet_key)?.with_chain_id(chain_id);
        let wallet_address = wallet.address();

        // provider is cloned, is connection shared?
        let signer = SignerMiddleware::new(provider.clone(), wallet);

        let contract_address = Address::from_slice(contract_address.as_ref());
        let contract = FUEL_STATE_CONTRACT::new(contract_address, Arc::new(signer));

        Ok(Self {
            provider,
            contract,
            wallet_address,
            commit_interval,
            metrics: Default::default(),
        })
    }

    async fn record_balance(&self) -> Result<()> {
        let balance = self
            .provider
            .get_balance(self.wallet_address, None)
            .await
            .map_err(|err| Error::NetworkError(err.to_string()))?;

        info!("wallet balance: {}", &balance);

        // Note: might lead to wrong metrics if we have more than 500k ETH
        let balance_gwei = balance / U256::from(1_000_000_000);
        self.metrics
            .eth_wallet_balance
            .set(balance_gwei.as_u64() as i64);

        Ok(())
    }

    fn calculate_commit_height(block_height: u32, commit_interval: u32) -> U256 {
        (block_height / commit_interval).into()
    }
}

impl RegistersMetrics for EthereumWs {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        self.metrics.metrics()
    }
}

#[async_trait]
impl EthereumAdapter for EthereumWs {
    async fn submit(&self, block: Block) -> Result<()> {
        let commit_height =
            Self::calculate_commit_height(block.header.height, self.commit_interval);
        let contract_call = self.contract.commit(*block.id, commit_height);
        let tx = contract_call
            .send()
            .await
            .map_err(|contract_err| match contract_err {
                ContractError::ProviderError { e } => Error::NetworkError(e.to_string()),
                ContractError::MiddlewareError { e } => Error::NetworkError(e.to_string()),
                _ => Error::Other(contract_err.to_string()),
            })?;

        info!("tx: {} submitted", tx.tx_hash());

        self.record_balance().await?;

        Ok(())
    }

    async fn get_latest_eth_block(&self) -> Result<U64> {
        self.provider.get_block_number().await.map_err(|err| {
            //self.handle_network_error();
            Error::NetworkError(err.to_string())
        })
    }

    fn event_streamer(&self, eth_block_height: u64) -> Box<dyn EventStreamer + Send + Sync> {
        let events = self
            .contract
            .event::<CommitSubmittedFilter>()
            .from_block(eth_block_height);

        Box::new(EthEventStreamer::new(events))
    }
}

#[derive(Clone)]
struct Metrics {
    eth_wallet_balance: IntGauge,
}

impl RegistersMetrics for Metrics {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        vec![Box::new(self.eth_wallet_balance.clone())]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let eth_wallet_balance = IntGauge::with_opts(Opts::new(
            "eth_wallet_balance",
            "Ethereum wallet balance [gwei].",
        ))
        .expect("eth_wallet_balance metric to be correctly configured");

        Self { eth_wallet_balance }
    }
}

#[cfg(test)]
mod tests {
    use fuels::{
        tx::Bytes32,
        types::block::{Block as FuelBlock, Header as FuelBlockHeader},
    };

    use super::*;

    #[test]
    fn calculates_correctly_the_commit_height() {
        assert_eq!(EthereumWs::calculate_commit_height(10, 3), 3.into());
    }

    async fn given_eth_rpc() -> EthereumWs {
        let url = Url::parse("http://127.0.0.42:42").unwrap();
        let wallet_key = "0x9e56ccf010fa4073274b8177ccaad46fbaf286645310d03ac9bb6afa922a7c36";
        EthereumWs::connect(&url, Default::default(), Default::default(), wallet_key, 3)
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
