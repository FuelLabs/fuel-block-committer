use std::{num::NonZeroU32, str::FromStr, sync::Arc};

use ethers::{
    prelude::{abigen, SignerMiddleware},
    providers::{Middleware, Provider, Ws},
    signers::{LocalWallet, Signer as _},
    types::{Address, BlockNumber, Chain, TransactionReceipt, H160, H256, U256, U64},
};
use ports::types::{TransactionResponse, ValidatedFuelBlock};
use serde_json::Value;
use url::Url;

use super::{event_streamer::EthEventStreamer, health_tracking_middleware::EthApi};
use crate::{
    eip_4844::{calculate_blob_fee, BlobSidecar, BlobTransaction, BlobTransactionEncoder},
    error::{Error, Result},
};

const STANDARD_GAS_LIMIT: u64 = 21000;

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
    blob_pool_wallet: Option<LocalWallet>,
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

    fn commit_interval(&self) -> NonZeroU32 {
        self.commit_interval
    }

    fn event_streamer(&self, eth_block_height: u64) -> EthEventStreamer {
        let events = self
            .contract
            .event::<CommitSubmittedFilter>()
            .from_block(eth_block_height);

        EthEventStreamer::new(events)
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>> {
        let tx_receipt = self.provider.get_transaction_receipt(tx_hash).await?;

        Self::convert_to_tx_response(tx_receipt)
    }

    async fn submit_l2_state(&self, state_data: Vec<u8>) -> Result<[u8; 32]> {
        let blob_pool_wallet = if let Some(blob_pool_wallet) = &self.blob_pool_wallet {
            blob_pool_wallet
        } else {
            return Err(Error::Other("blob pool wallet not configured".to_string()));
        };

        let sidecar = BlobSidecar::new(state_data).map_err(|e| Error::Other(e.to_string()))?;
        let blob_tx = self
            .prepare_blob_tx(
                sidecar.versioned_hashes(),
                blob_pool_wallet.address(),
                blob_pool_wallet.chain_id(),
            )
            .await?;

        let tx_encoder = BlobTransactionEncoder::new(blob_tx, sidecar);
        let (tx_hash, raw_tx) = tx_encoder.raw_signed_w_sidecar(blob_pool_wallet);

        self.provider.send_raw_transaction(raw_tx.into()).await?;

        Ok(tx_hash.to_fixed_bytes())
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
        blob_pool_wallet_key: Option<String>,
    ) -> Result<Self> {
        let provider = Provider::<Ws>::connect(url.to_string()).await?;

        let wallet = LocalWallet::from_str(wallet_key)?.with_chain_id(chain_id);
        let address = wallet.address();

        let blob_pool_wallet = blob_pool_wallet_key
            .map(|key| LocalWallet::from_str(&key))
            .transpose()?
            .map(|wallet| wallet.with_chain_id(chain_id));

        let signer = SignerMiddleware::new(provider.clone(), wallet.clone());

        let contract_address = Address::from_slice(contract_address.as_ref());
        let contract = FUEL_STATE_CONTRACT::new(contract_address, Arc::new(signer));

        let interval_u256 = contract.blocks_per_commit_interval().call().await?;

        let commit_interval = u32::try_from(interval_u256)
            .map_err(|e| Error::Other(e.to_string()))
            .and_then(|value| {
                NonZeroU32::new(value).ok_or_else(|| {
                    Error::Other("l1 contract reported a commit interval of 0".to_string())
                })
            })?;

        Ok(Self {
            provider,
            contract,
            commit_interval,
            address,
            blob_pool_wallet,
        })
    }

    pub(crate) fn calculate_commit_height(block_height: u32, commit_interval: NonZeroU32) -> U256 {
        (block_height / commit_interval).into()
    }

    async fn _balance(&self, address: H160) -> Result<U256> {
        Ok(self.provider.get_balance(address, None).await?)
    }

    async fn prepare_blob_tx(
        &self,
        blob_versioned_hashes: Vec<H256>,
        address: H160,
        chain_id: u64,
    ) -> Result<BlobTransaction> {
        let nonce = self.provider.get_transaction_count(address, None).await?;

        let (max_fee_per_gas, max_priority_fee_per_gas) =
            self.provider.estimate_eip1559_fees(None).await?;

        let gas_limit = U256::from(STANDARD_GAS_LIMIT);

        let max_fee_per_blob_gas = self.calculate_blob_fee(blob_versioned_hashes.len()).await?;

        let blob_tx = BlobTransaction {
            to: address,
            chain_id: chain_id.into(),
            gas_limit,
            nonce,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            max_fee_per_blob_gas,
            blob_versioned_hashes,
        };

        Ok(blob_tx)
    }

    async fn calculate_blob_fee(&self, num_blobs: usize) -> Result<U256> {
        let latest = self
            .provider
            .get_block(BlockNumber::Latest)
            .await?
            .expect("block not found");

        let excess_blob_gas = latest.excess_blob_gas.expect("excess blob gas not found");
        let max_fee_per_blob_gas = calculate_blob_fee(excess_blob_gas, num_blobs as u64);

        Ok(max_fee_per_blob_gas)
    }

    fn convert_to_tx_response(
        tx_receipt: Option<TransactionReceipt>,
    ) -> Result<Option<TransactionResponse>> {
        let Some(tx_receipt) = tx_receipt else {
            return Ok(None);
        };

        let block_number = Self::extract_block_number_from_receipt(&tx_receipt)?;

        const SUCCESS_STATUS: u64 = 1;
        // Only present after activation of [EIP-658](https://eips.ethereum.org/EIPS/eip-658)
        let Some(status) = tx_receipt.status else {
            return Err(Error::Other(
                "`status` not present in tx receipt".to_string(),
            ));
        };

        let status: u64 = status.try_into().map_err(|_| {
            Error::Other("could not convert tx receipt `status` to `u64`".to_string())
        })?;

        Ok(Some(TransactionResponse::new(
            block_number,
            status == SUCCESS_STATUS,
        )))
    }

    fn extract_block_number_from_receipt(receipt: &TransactionReceipt) -> Result<u64> {
        receipt
            .block_number
            .ok_or_else(|| {
                Error::Other("transaction receipt does not contain block number".to_string())
            })?
            .try_into()
            .map_err(|_| Error::Other("could not convert `block_number` to `u64`".to_string()))
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
