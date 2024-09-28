use std::{cmp::min, num::NonZeroU32};

use alloy::{
    eips::eip4844::BYTES_PER_BLOB,
    network::{Ethereum, EthereumWallet, TransactionBuilder, TransactionBuilder4844, TxSigner},
    primitives::{Address, U256},
    providers::{Provider, ProviderBuilder, WsConnect},
    pubsub::PubSubFrontend,
    rpc::types::{TransactionReceipt, TransactionRequest},
    signers::aws::AwsSigner,
    sol,
};
use itertools::Itertools;
use metrics::{
    prometheus::{self, histogram_opts},
    RegistersMetrics,
};
use ports::{
    l1::FragmentsSubmitted,
    types::{Fragment, NonEmpty, TransactionResponse},
};
use url::Url;

use super::{event_streamer::EthEventStreamer, health_tracking_middleware::EthApi};
use crate::{
    error::{Error, Result},
    Eip4844BlobEncoder,
};

pub type WsProvider = alloy::providers::fillers::FillProvider<
    alloy::providers::fillers::JoinFill<
        alloy::providers::fillers::JoinFill<
            alloy::providers::Identity,
            alloy::providers::fillers::JoinFill<
                alloy::providers::fillers::GasFiller,
                alloy::providers::fillers::JoinFill<
                    alloy::providers::fillers::BlobGasFiller,
                    alloy::providers::fillers::JoinFill<
                        alloy::providers::fillers::NonceFiller,
                        alloy::providers::fillers::ChainIdFiller,
                    >,
                >,
            >,
        >,
        alloy::providers::fillers::WalletFiller<EthereumWallet>,
    >,
    alloy::providers::RootProvider<alloy::pubsub::PubSubFrontend>,
    alloy::pubsub::PubSubFrontend,
    Ethereum,
>;
type FuelStateContract = IFuelStateContract::IFuelStateContractInstance<PubSubFrontend, WsProvider>;

sol!(
    #[sol(rpc)]
    interface IFuelStateContract {
        function commit(bytes32 blockHash, uint256 commitHeight) external whenNotPaused;
        event CommitSubmitted(uint256 indexed commitHeight, bytes32 blockHash);
        function finalized(bytes32 blockHash, uint256 blockHeight) external view whenNotPaused returns (bool);
        function blockHashAtCommit(uint256 commitHeight) external view returns (bytes32);
        function BLOCKS_PER_COMMIT_INTERVAL() external view returns (uint256);
    }
);

#[derive(Clone)]
pub struct WsConnection {
    provider: WsProvider,
    blob_provider: Option<WsProvider>,
    address: Address,
    blob_signer_address: Option<Address>,
    contract: FuelStateContract,
    commit_interval: NonZeroU32,
    metrics: Metrics,
}

impl RegistersMetrics for WsConnection {
    fn metrics(&self) -> Vec<Box<dyn metrics::prometheus::core::Collector>> {
        vec![
            Box::new(self.metrics.blobs_per_tx.clone()),
            Box::new(self.metrics.blob_used_bytes.clone()),
        ]
    }
}

#[derive(Clone)]
struct Metrics {
    blobs_per_tx: prometheus::Histogram,
    blob_used_bytes: prometheus::Histogram,
}

fn custom_exponential_buckets(start: f64, end: f64, steps: usize) -> Vec<f64> {
    let factor = (end / start).powf(1.0 / (steps - 1) as f64);
    let mut buckets = Vec::with_capacity(steps);

    let mut value = start;
    for _ in 0..(steps - 1) {
        buckets.push(value.ceil());
        value *= factor;
    }

    buckets.push(end.ceil());

    buckets
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            blobs_per_tx: prometheus::Histogram::with_opts(histogram_opts!(
                "blob_per_tx",
                "Number of blobs per blob transaction",
                vec![1.0f64, 2., 3., 4., 5., 6.]
            ))
            .expect("to be correctly configured"),

            blob_used_bytes: prometheus::Histogram::with_opts(histogram_opts!(
                "blob_utilization",
                "bytes filled per blob",
                custom_exponential_buckets(1000f64, BYTES_PER_BLOB as f64, 20)
            ))
            .expect("to be correctly configured"),
        }
    }
}

#[async_trait::async_trait]
impl EthApi for WsConnection {
    async fn submit(&self, hash: [u8; 32], height: u32) -> Result<()> {
        let commit_height = Self::calculate_commit_height(height, self.commit_interval);
        let contract_call = self.contract.commit(hash.into(), commit_height);
        let tx = contract_call.send().await?;
        tracing::info!("tx: {} submitted", tx.tx_hash());

        Ok(())
    }

    async fn get_block_number(&self) -> Result<u64> {
        let response = self.provider.get_block_number().await?;
        Ok(response)
    }

    async fn balance(&self) -> Result<U256> {
        let address = self.address;
        Ok(self.provider.get_balance(address).await?)
    }

    fn commit_interval(&self) -> NonZeroU32 {
        self.commit_interval
    }

    fn event_streamer(&self, eth_block_height: u64) -> EthEventStreamer {
        let filter = self
            .contract
            .CommitSubmitted_filter()
            .from_block(eth_block_height)
            .filter;
        EthEventStreamer::new(filter, self.contract.provider().clone())
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>> {
        let tx_receipt = self
            .provider
            .get_transaction_receipt(tx_hash.into())
            .await?;

        Self::convert_to_tx_response(tx_receipt)
    }

    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
    ) -> Result<ports::l1::FragmentsSubmitted> {
        let (blob_provider, blob_signer_address) =
            match (&self.blob_provider, &self.blob_signer_address) {
                (Some(provider), Some(address)) => (provider, address),
                _ => return Err(Error::Other("blob pool signer not configured".to_string())),
            };

        // we only want to add it to the metrics if the submission succeeds
        let used_bytes_per_fragment = fragments.iter().map(|f| f.used_bytes()).collect_vec();

        let num_fragments = min(fragments.len(), 6);

        let limited_fragments = fragments.into_iter().take(num_fragments);
        let sidecar = Eip4844BlobEncoder::decode(limited_fragments)?;

        let blob_tx = TransactionRequest::default()
            .with_blob_sidecar(sidecar)
            .with_to(*blob_signer_address);

        let tx = blob_provider.send_transaction(blob_tx).await?;
        self.metrics.blobs_per_tx.observe(num_fragments as f64);

        for bytes in used_bytes_per_fragment {
            self.metrics.blob_used_bytes.observe(bytes as f64);
        }

        Ok(FragmentsSubmitted {
            tx: tx.tx_hash().0,
            num_fragments: num_fragments.try_into().expect("cannot be zero"),
        })
    }

    #[cfg(feature = "test-helpers")]
    async fn finalized(&self, hash: [u8; 32], height: u32) -> Result<bool> {
        Ok(self
            .contract
            .finalized(hash.into(), U256::from(height))
            .call()
            .await?
            ._0)
    }

    #[cfg(feature = "test-helpers")]
    async fn block_hash_at_commit_height(&self, commit_height: u32) -> Result<[u8; 32]> {
        Ok(self
            .contract
            .blockHashAtCommit(U256::from(commit_height))
            .call()
            .await?
            ._0
            .into())
    }
}

impl WsConnection {
    pub async fn connect(
        url: Url,
        contract_address: Address,
        main_signer: AwsSigner,
        blob_signer: Option<AwsSigner>,
    ) -> Result<Self> {
        let address = main_signer.address();

        let ws = WsConnect::new(url);
        let provider = Self::provider_with_signer(ws.clone(), main_signer).await?;

        let (blob_provider, blob_signer_address) = if let Some(signer) = blob_signer {
            let blob_signer_address = signer.address();
            let blob_provider = Self::provider_with_signer(ws, signer).await?;
            (Some(blob_provider), Some(blob_signer_address))
        } else {
            (None, None)
        };

        let contract_address = Address::from_slice(contract_address.as_ref());
        let contract = FuelStateContract::new(contract_address, provider.clone());

        let interval_u256 = contract.BLOCKS_PER_COMMIT_INTERVAL().call().await?._0;

        let commit_interval = u32::try_from(interval_u256)
            .map_err(|e| Error::Other(e.to_string()))
            .and_then(|value| {
                NonZeroU32::new(value).ok_or_else(|| {
                    Error::Other("l1 contract reported a commit interval of 0".to_string())
                })
            })?;

        Ok(Self {
            provider,
            blob_provider,
            address,
            blob_signer_address,
            contract,
            commit_interval,
            metrics: Default::default(),
        })
    }

    async fn provider_with_signer(ws: WsConnect, signer: AwsSigner) -> Result<WsProvider> {
        let wallet = EthereumWallet::from(signer);
        ProviderBuilder::new()
            .with_recommended_fillers()
            .wallet(wallet)
            .on_ws(ws)
            .await
            .map_err(Into::into)
    }

    pub(crate) fn calculate_commit_height(block_height: u32, commit_interval: NonZeroU32) -> U256 {
        U256::from(block_height / commit_interval)
    }

    async fn _balance(&self, address: Address) -> Result<U256> {
        Ok(self.provider.get_balance(address).await?)
    }

    fn convert_to_tx_response(
        tx_receipt: Option<TransactionReceipt>,
    ) -> Result<Option<TransactionResponse>> {
        let Some(tx_receipt) = tx_receipt else {
            return Ok(None);
        };

        let block_number = Self::extract_block_number_from_receipt(&tx_receipt)?;

        Ok(Some(TransactionResponse::new(
            block_number,
            tx_receipt.status(),
        )))
    }

    fn extract_block_number_from_receipt(receipt: &TransactionReceipt) -> Result<u64> {
        receipt.block_number.ok_or_else(|| {
            Error::Other("transaction receipt does not contain block number".to_string())
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn calculates_correctly_the_commit_height() {
        assert_eq!(
            WsConnection::calculate_commit_height(10, 3.try_into().unwrap()),
            U256::from(3)
        );
    }
}
