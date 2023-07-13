use std::{num::NonZeroU32, str::FromStr, sync::Arc};

use async_trait::async_trait;
use ethers::{
    prelude::{abigen, ContractError, SignerMiddleware},
    providers::{Middleware, Provider, Ws},
    signers::{LocalWallet, Signer},
    types::{Address, Chain, U256, U64},
};
use prometheus::{IntGauge, Opts};
use serde_json::Value;
use tracing::info;
use url::Url;

use crate::{
    adapters::{
        ethereum_adapter::{
            websocket::event_streamer::EthEventStreamer, EthereumAdapter, EventStreamer,
        },
        fuel_adapter::FuelBlock,
    },
    errors::{Error, Result},
    telemetry::RegistersMetrics,
};

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
    commit_interval: NonZeroU32,
    metrics: Metrics,
}

impl EthereumWs {
    pub async fn connect(
        ethereum_rpc: &Url,
        chain_id: Chain,
        contract_address: Address,
        ethereum_wallet_key: &str,
        commit_interval: NonZeroU32,
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

    fn calculate_commit_height(block_height: u32, commit_interval: NonZeroU32) -> U256 {
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
    async fn submit(&self, block: FuelBlock) -> Result<()> {
        let commit_height = Self::calculate_commit_height(block.height, self.commit_interval);
        let contract_call = self.contract.commit(block.hash, commit_height);
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

    async fn get_block_number(&self) -> Result<u64> {
        // if provider.get_block_number is used the outgoing JSON RPC request would have the
        // 'params' field set as `params: null`. This is accepted by Anvil but rejected by hardhat.
        // By passing a preconstructed serde_json Value::Array it will cause params to be defined
        // as `params: []` which is acceptable by both Anvil and Hardhat.
        self.provider
            .request("eth_blockNumber", Value::Array(vec![]))
            .await
            .map_err(|err| Error::NetworkError(err.to_string()))
            .map(|height: U64| height.as_u64())
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
    use super::*;

    #[test]
    fn calculates_correctly_the_commit_height() {
        assert_eq!(
            EthereumWs::calculate_commit_height(10, 3.try_into().unwrap()),
            3.into()
        );
    }
}
