use std::{num::NonZeroU32, str::FromStr, sync::Arc};

use ethers::{
    prelude::{abigen, SignerMiddleware},
    providers::{Middleware, Provider, Ws},
    signers::{LocalWallet, Signer},
    types::{Address, Chain, H160, U256, U64},
};
use ports::types::ValidatedFuelBlock;
use serde_json::Value;
use url::Url;

use super::{event_streamer::EthEventStreamer, health_tracking_middleware::EthApi};
use crate::error::{Error, Result};

abigen!(
    FUEL_STATE_CONTRACT,
    r#"[
        function commit(bytes32 blockHash, uint256 commitHeight) external whenNotPaused
        event CommitSubmitted(uint256 indexed commitHeight, bytes32 blockHash)
        function finalized(bytes32 blockHash, uint256 blockHeight) external view whenNotPaused returns (bool)
        function blockHashAtCommit(uint256 commitHeight) external view returns (bytes32)
        function BLOCKS_PER_COMMIT_INTERVAL() external view returns (uint256)
    ]"#,
);

#[derive(Clone)]
pub struct WsConnection {
    provider: Provider<Ws>,
    contract: FUEL_STATE_CONTRACT<SignerMiddleware<Provider<Ws>, LocalWallet>>,
    commit_interval: NonZeroU32,
    address: H160,
}

#[async_trait::async_trait]
impl EthApi for WsConnection {
    async fn submit(&self, block: ValidatedFuelBlock) -> Result<()> {
        let commit_height = Self::calculate_commit_height(block.height(), self.commit_interval);
        let contract_call = self.contract.commit(block.hash(), commit_height);
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

    fn commit_interval(&self) -> Result<NonZeroU32> {
        Ok(self.commit_interval)
    }

    fn event_streamer(&self, eth_block_height: u64) -> EthEventStreamer {
        let events = self
            .contract
            .event::<CommitSubmittedFilter>()
            .from_block(eth_block_height);

        EthEventStreamer::new(events)
    }

    #[cfg(feature = "test-helpers")]
    async fn finalized(&self, block: ValidatedFuelBlock) -> Result<bool> {
        Ok(self
            .contract
            .finalized(block.hash(), block.height().into())
            .call()
            .await?)
    }

    #[cfg(feature = "test-helpers")]
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
        url: &Url,
        chain_id: Chain,
        contract_address: Address,
        wallet_key: &str,
    ) -> Result<Self> {
        let provider = Provider::<Ws>::connect(url.to_string()).await?;

        let wallet = LocalWallet::from_str(wallet_key)?.with_chain_id(chain_id);
        let address = wallet.address();

        let signer = SignerMiddleware::new(provider.clone(), wallet);

        let contract_address = Address::from_slice(contract_address.as_ref());
        let contract = FUEL_STATE_CONTRACT::new(contract_address, Arc::new(signer));

        let interval_u256 = contract.blocks_per_commit_interval().call().await?;

        let commit_interval = u32::try_from(interval_u256)
            .map_err(|e| Error::Other(e.to_string()))
            .and_then(|value| {
                NonZeroU32::new(value)
                    .ok_or_else(|| Error::Other("l1 contract reported a commit interval of 0"))
            })?;

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
