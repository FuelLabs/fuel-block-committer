use std::{num::NonZeroU32, str::FromStr, sync::Arc};

use async_trait::async_trait;
use ethers::{
    prelude::{abigen, ContractError, SignerMiddleware},
    providers::{Middleware, Provider, Ws},
    signers::{LocalWallet, Signer},
    types::{Address, Chain, H160, U256, U64},
};
use serde_json::Value;
use tracing::info;
use url::Url;

use crate::{
    adapters::{
        block_fetcher::FuelBlock,
        ethereum_adapter::{
            websocket::event_streamer::EthEventStreamer, EthereumAdapter, EventStreamer,
        },
    },
    errors::{Error, Result},
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
    commit_interval: NonZeroU32,
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
        let _wallet_address = wallet.address();

        let signer = SignerMiddleware::new(provider.clone(), wallet);

        let contract_address = Address::from_slice(contract_address.as_ref());
        let contract = FUEL_STATE_CONTRACT::new(contract_address, Arc::new(signer));

        Ok(Self {
            provider,
            contract,
            commit_interval,
        })
    }

    fn calculate_commit_height(block_height: u32, commit_interval: NonZeroU32) -> U256 {
        (block_height / commit_interval).into()
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

    async fn balance(&self, address: H160) -> Result<U256> {
        self.provider
            .get_balance(address, None)
            .await
            .map_err(|err| Error::NetworkError(err.to_string()))
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
