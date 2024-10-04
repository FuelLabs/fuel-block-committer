use std::{
    cmp::min,
    num::NonZeroU32,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use alloy::{
    eips::eip4844::BYTES_PER_BLOB,
    network::{Ethereum, EthereumWallet, TransactionBuilder, TransactionBuilder4844, TxSigner},
    primitives::{Address, U256},
    providers::{utils::Eip1559Estimation, Provider, ProviderBuilder, SendableTx, WsConnect},
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
    types::{BlockSubmissionTx, Fragment, NonEmpty, TransactionResponse},
};
use tracing::info;
use url::Url;

use super::health_tracking_middleware::EthApi;
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
    first_tx_gas_estimation_multiplier: Option<u64>,
    first_blob_tx_sent: Arc<AtomicBool>,
    blob_provider: Option<WsProvider>,
    blob_signer_address: Option<Address>,
    contract: FuelStateContract,
    commit_interval: NonZeroU32,
    metrics: Metrics,
}

impl RegistersMetrics for WsConnection {
    fn metrics(&self) -> Vec<Box<dyn metrics::prometheus::core::Collector>> {
        vec![
            Box::new(self.metrics.blobs_per_tx.clone()),
            Box::new(self.metrics.blob_unused_bytes.clone()),
        ]
    }
}

#[derive(Clone)]
struct Metrics {
    blobs_per_tx: prometheus::Histogram,
    blob_unused_bytes: prometheus::Histogram,
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

            blob_unused_bytes: prometheus::Histogram::with_opts(histogram_opts!(
                "blob_utilization",
                "unused bytes per blob",
                metrics::custom_exponential_buckets(1000f64, BYTES_PER_BLOB as f64, 20)
            ))
            .expect("to be correctly configured"),
        }
    }
}

#[async_trait::async_trait]
impl EthApi for WsConnection {
    async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx> {
        let commit_height = Self::calculate_commit_height(height, self.commit_interval);

        let contract_call = self.contract.commit(hash.into(), commit_height);
        let tx_request = contract_call.into_transaction_request();

        let Eip1559Estimation {
            max_fee_per_gas,
            max_priority_fee_per_gas,
        } = self.provider.estimate_eip1559_fees(None).await?;
        let nonce = self.provider.get_transaction_count(self.address).await?;
        let tx_request = tx_request
            .max_fee_per_gas(max_fee_per_gas)
            .max_priority_fee_per_gas(max_priority_fee_per_gas)
            .nonce(nonce);

        let tx = self.provider.send_transaction(tx_request).await?;
        tracing::info!("tx: {} submitted", tx.tx_hash());

        let submission_tx = BlockSubmissionTx {
            hash: tx.tx_hash().0,
            nonce: nonce as u32, // TODO: conversion
            max_fee: max_fee_per_gas,
            priority_fee: max_priority_fee_per_gas,
            ..Default::default()
        };

        Ok(submission_tx)
    }

    async fn get_block_number(&self) -> Result<u64> {
        let response = self.provider.get_block_number().await?;
        Ok(response)
    }

    async fn balance(&self, address: Address) -> Result<U256> {
        Ok(self.provider.get_balance(address).await?)
    }

    fn commit_interval(&self) -> NonZeroU32 {
        self.commit_interval
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
        let unused_bytes_per_fragment = fragments.iter().map(|f| f.unused_bytes).collect_vec();

        let num_fragments = min(fragments.len(), 6);

        let limited_fragments = fragments.into_iter().take(num_fragments);
        let sidecar = Eip4844BlobEncoder::decode(limited_fragments)?;

        let blob_tx = match (
            self.first_blob_tx_sent.load(Ordering::Relaxed),
            self.first_tx_gas_estimation_multiplier,
        ) {
            (false, Some(gas_estimation_multiplier)) => {
                let max_fee_per_blob_gas = blob_provider.get_blob_base_fee().await?;
                let Eip1559Estimation {
                    max_fee_per_gas,
                    max_priority_fee_per_gas,
                } = blob_provider.estimate_eip1559_fees(None).await?;

                TransactionRequest::default()
                    .with_max_fee_per_blob_gas(
                        max_fee_per_blob_gas.saturating_mul(gas_estimation_multiplier.into()),
                    )
                    .with_max_fee_per_gas(
                        max_fee_per_gas.saturating_mul(gas_estimation_multiplier.into()),
                    )
                    .with_max_priority_fee_per_gas(
                        max_priority_fee_per_gas.saturating_mul(gas_estimation_multiplier.into()),
                    )
                    .with_blob_sidecar(sidecar)
                    .with_to(*blob_signer_address)
            }
            _ => TransactionRequest::default()
                .with_blob_sidecar(sidecar)
                .with_to(*blob_signer_address),
        };

        let blob_tx = blob_provider.fill(blob_tx).await?;
        let SendableTx::Envelope(blob_tx) = blob_tx else {
            return Err(crate::error::Error::Other(
                "Expected an envelope because we have a wallet filler as well, but got a builder from alloy. This is a bug.".to_string(),
            ));
        };
        let tx_id = *blob_tx.tx_hash();
        info!("sending blob tx: {tx_id}",);

        let _ = blob_provider.send_tx_envelope(blob_tx).await?;

        self.first_blob_tx_sent.store(true, Ordering::Relaxed);

        self.metrics.blobs_per_tx.observe(num_fragments as f64);

        for bytes in unused_bytes_per_fragment {
            self.metrics.blob_unused_bytes.observe(bytes.into());
        }

        Ok(FragmentsSubmitted {
            tx: tx_id.0,
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
        first_tx_gas_estimation_multiplier: Option<u64>,
    ) -> Result<Self> {
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
            blob_signer_address,
            contract,
            commit_interval,
            metrics: Default::default(),
            first_blob_tx_sent: Arc::new(AtomicBool::new(false)),
            first_tx_gas_estimation_multiplier,
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
