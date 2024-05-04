use std::{num::NonZeroU32, str::FromStr, sync::Arc};

use ethers::{
    prelude::{abigen, SignerMiddleware},
    providers::{Middleware, Provider, Ws},
    signers::{LocalWallet, Signer},
    types::{Address, Chain, H160, U256, U64},
};
use ports::FuelBlock;
use serde_json::Value;
use url::Url;

use super::{event_streamer::EthEventStreamer, health_tracking_middleware::MyAdapter};
use crate::Result;

abigen!(
    FUEL_STATE_CONTRACT,
    r#"[
        function commit(bytes32 blockHash, uint256 commitHeight) external whenNotPaused
        event CommitSubmitted(uint256 indexed commitHeight, bytes32 blockHash)
        function finalized(bytes32 blockHash, uint256 blockHeight) external view whenNotPaused returns (bool)
        function blockHashAtCommit(uint256 commitHeight) external view returns (bytes32)
    ]"#,
);

#[derive(Clone)]
pub struct WsConnection {
    provider: Provider<Ws>,
    pub(crate) contract: FUEL_STATE_CONTRACT<SignerMiddleware<Provider<Ws>, LocalWallet>>,
    commit_interval: NonZeroU32,
    address: H160,
}

#[async_trait::async_trait]
impl MyAdapter for WsConnection {
    async fn submit(&self, block: FuelBlock) -> Result<()> {
        let commit_height = Self::calculate_commit_height(block.height, self.commit_interval);
        let contract_call = self.contract.commit(block.hash, commit_height);
        let tx = contract_call.send().await?;

        tracing::info!("tx: {} submitted", tx.tx_hash());

        Ok(())
    }

    async fn get_block_number(&self) -> Result<u64> {
        // if provider.get_block_number is used the outgoing JSON RPC request would have the
        // 'params' field set as `params: null`. This is accepted by Anvil but rejected by hardhat.
        // By passing a preconstructed serde_json Value::Array it will cause params to be defined
        // as `params: []` which is acceptable by both Anvil and Hardhat.
        let response = self
            .provider
            .request::<Value, U64>("eth_blockNumber", Value::Array(vec![]))
            .await?;
        Ok(response.as_u64())
    }

    async fn balance(&self) -> Result<U256> {
        let address = self.address;
        Ok(self.provider.get_balance(address, None).await?)
    }

    fn event_streamer(&self, eth_block_height: u64) -> EthEventStreamer {
        let events = self
            .contract
            .event::<CommitSubmittedFilter>()
            .from_block(eth_block_height);

        EthEventStreamer::new(events)
    }

    async fn finalized(&self, block: FuelBlock) -> Result<bool> {
        Ok(self
            .contract
            .finalized(block.hash, block.height.into())
            .call()
            .await?)
    }

    async fn block_hash_at_commit_height(&self, commit_height: u32) -> Result<[u8; 32]> {
        Ok(self
            .contract
            .block_hash_at_commit(commit_height.into())
            .call()
            .await?)
    }
}

impl WsConnection {
    pub async fn connect(
        ethereum_rpc: &Url,
        chain_id: Chain,
        contract_address: Address,
        ethereum_wallet_key: &str,
        commit_interval: NonZeroU32,
    ) -> Result<Self> {
        let provider = Provider::<Ws>::connect(ethereum_rpc.to_string()).await?;

        let wallet = LocalWallet::from_str(ethereum_wallet_key)?.with_chain_id(chain_id);
        let address = wallet.address();

        let signer = SignerMiddleware::new(provider.clone(), wallet);

        let contract_address = Address::from_slice(contract_address.as_ref());
        let contract = FUEL_STATE_CONTRACT::new(contract_address, Arc::new(signer));

        Ok(Self {
            provider,
            contract,
            commit_interval,
            address,
        })
    }

    pub(crate) fn calculate_commit_height(block_height: u32, commit_interval: NonZeroU32) -> U256 {
        (block_height / commit_interval).into()
    }

    async fn _balance(&self, address: H160) -> Result<U256> {
        Ok(self.provider.get_balance(address, None).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_correctly_the_commit_height() {
        assert_eq!(
            WsConnection::calculate_commit_height(10, 3.try_into().unwrap()),
            3.into()
        );
    }
}
