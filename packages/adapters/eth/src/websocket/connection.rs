use std::{
    cmp::{max, min},
    num::NonZeroU32,
    time::Duration,
};

use alloy::{
    consensus::Transaction,
    eips::{
        eip4844::{BYTES_PER_BLOB, DATA_GAS_PER_BLOB},
        BlockNumberOrTag,
    },
    network::{Ethereum, EthereumWallet, TransactionBuilder, TransactionBuilder4844, TxSigner},
    primitives::{Address, U256},
    providers::{utils::Eip1559Estimation, Provider, ProviderBuilder, SendableTx, WsConnect},
    pubsub::PubSubFrontend,
    rpc::types::{TransactionReceipt, TransactionRequest},
    sol,
};
use itertools::Itertools;
use metrics::{
    prometheus::{self, histogram_opts},
    RegistersMetrics,
};
use services::types::{
    BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx, NonEmpty, TransactionResponse,
};
use tracing::info;
use url::Url;

use super::{health_tracking_middleware::EthApi, Signers};
use crate::{
    error::{Error, Result},
    BlobEncoder,
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
    main_address: Address,
    blob_provider: Option<WsProvider>,
    blob_signer_address: Option<Address>,
    contract: FuelStateContract,
    commit_interval: NonZeroU32,
    tx_max_fee: u128,
    send_tx_request_timeout: Duration,
    metrics: Metrics,
}

impl WsConnection {
    async fn get_next_blob_fee(&self, provider: &WsProvider) -> Result<u128> {
        provider
            .get_block_by_number(BlockNumberOrTag::Latest, false)
            .await?
            .ok_or(Error::Network(
                "get_block_by_number returned None".to_string(),
            ))?
            .header
            .next_block_blob_fee()
            .ok_or(Error::Network(
                "next_block_blob_fee returned None".to_string(),
            ))
    }

    async fn get_bumped_fees(
        &self,
        previous_tx: &L1Tx,
        provider: &WsProvider,
    ) -> Result<(u128, u128, u128)> {
        let next_blob_fee = self.get_next_blob_fee(provider).await?;
        let max_fee_per_blob_gas = max(next_blob_fee, previous_tx.blob_fee.saturating_mul(2));

        let Eip1559Estimation {
            max_fee_per_gas,
            max_priority_fee_per_gas,
        } = provider.estimate_eip1559_fees(None).await?;

        let max_fee_per_gas = max(max_fee_per_gas, previous_tx.max_fee.saturating_mul(2));
        let max_priority_fee_per_gas = max(
            max_priority_fee_per_gas,
            previous_tx.priority_fee.saturating_mul(2),
        );

        Ok((
            max_fee_per_gas,
            max_priority_fee_per_gas,
            max_fee_per_blob_gas,
        ))
    }

    fn get_max_fee(tx: &L1Tx, gas_limit: u128, num_fragments: usize) -> u128 {
        tx.max_fee.saturating_mul(gas_limit).saturating_add(
            tx.blob_fee
                .saturating_mul(num_fragments as u128)
                .saturating_mul(DATA_GAS_PER_BLOB as u128),
        )
    }
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
                "blob_unused_bytes",
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

        let nonce = self
            .provider
            .get_transaction_count(self.main_address)
            .await?;
        let tx_request = tx_request
            .max_fee_per_gas(max_fee_per_gas)
            .max_priority_fee_per_gas(max_priority_fee_per_gas)
            .nonce(nonce);

        let send_fut = self.provider.send_transaction(tx_request);
        let tx = tokio::time::timeout(self.send_tx_request_timeout, send_fut)
            .await
            .map_err(|_| Error::Network("timed out trying to submit block".to_string()))??;
        tracing::info!("tx: {} submitted", tx.tx_hash());

        let nonce = nonce.try_into().map_err(|_| {
            Error::Other(
                "could not convert `u64` nonce to `u32` when storing `BlockSubmissionTx`"
                    .to_string(),
            )
        })?;

        let submission_tx = BlockSubmissionTx {
            hash: tx.tx_hash().0,
            nonce,
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

    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> Result<bool> {
        Ok(self
            .provider
            .get_transaction_by_hash(tx_hash.into())
            .await?
            .is_none())
    }

    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<L1Tx>,
    ) -> Result<(L1Tx, services::types::FragmentsSubmitted)> {
        let (blob_provider, blob_signer_address) =
            match (&self.blob_provider, &self.blob_signer_address) {
                (Some(provider), Some(address)) => (provider, address),
                _ => return Err(Error::Other("blob pool signer not configured".to_string())),
            };

        // we only want to add it to the metrics if the submission succeeds
        let unused_bytes_per_fragment = fragments.iter().map(|f| f.unused_bytes).collect_vec();

        let num_fragments = min(fragments.len(), 6);

        let limited_fragments = fragments.into_iter().take(num_fragments);
        let sidecar = BlobEncoder::sidecar_from_fragments(limited_fragments)?;

        let blob_tx = match previous_tx {
            Some(previous_tx) => {
                let (max_fee_per_gas, max_priority_fee_per_gas, max_fee_per_blob_gas) =
                    self.get_bumped_fees(&previous_tx, blob_provider).await?;

                TransactionRequest::default()
                    .with_max_fee_per_gas(max_fee_per_gas)
                    .with_max_priority_fee_per_gas(max_priority_fee_per_gas)
                    .with_max_fee_per_blob_gas(max_fee_per_blob_gas)
                    .with_nonce(previous_tx.nonce as u64)
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

        let nonce = blob_tx.nonce().try_into().map_err(|_| {
            Error::Other(
                "could not convert `u64` blob_tx nonce to `u32` when creating `L1Tx`".to_string(),
            )
        })?;

        let l1_tx = L1Tx {
            hash: tx_id.0,
            nonce,
            max_fee: blob_tx.max_fee_per_gas(),
            priority_fee: blob_tx
                .max_priority_fee_per_gas()
                .expect("eip4844 tx to have priority fee"),
            blob_fee: blob_tx
                .max_fee_per_blob_gas()
                .expect("eip4844 tx to have blob fee"),
            ..Default::default()
        };

        info!("sending blob tx: {tx_id} with nonce: {}, max_fee_per_gas: {}, tip: {}, max_blob_fee_per_gas: {}", l1_tx.nonce, l1_tx.max_fee, l1_tx.priority_fee, l1_tx.blob_fee);

        let max_fee = WsConnection::get_max_fee(&l1_tx, blob_tx.gas_limit(), num_fragments);
        if max_fee > self.tx_max_fee {
            return Err(Error::Other(
                format!(
                    "max fee exceeded: tried {}, limit {}",
                    max_fee, self.tx_max_fee
                )
                .to_string(),
            ));
        }

        let send_fut = blob_provider.send_tx_envelope(blob_tx);
        let _ = tokio::time::timeout(self.send_tx_request_timeout, send_fut)
            .await
            .map_err(|_| Error::Network("timed out trying to send blob tx".to_string()))??;

        self.metrics.blobs_per_tx.observe(num_fragments as f64);

        for bytes in unused_bytes_per_fragment {
            self.metrics.blob_unused_bytes.observe(bytes.into());
        }

        let fragments_submitted = FragmentsSubmitted {
            num_fragments: num_fragments.try_into().expect("cannot be zero"),
        };

        Ok((l1_tx, fragments_submitted))
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
        signers: Signers,
        tx_max_fee: u128,
        send_tx_request_timeout: Duration,
    ) -> Result<Self> {
        let address = TxSigner::address(&signers.main);
        let ws = WsConnect::new(url);
        let provider = Self::provider_with_signer(ws.clone(), signers.main).await?;

        let (blob_provider, blob_signer_address) = if let Some(signer) = signers.blob {
            let blob_signer_address = TxSigner::address(&signer);
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
            main_address: address,
            blob_provider,
            blob_signer_address,
            contract,
            commit_interval,
            tx_max_fee,
            send_tx_request_timeout,
            metrics: Default::default(),
        })
    }

    async fn provider_with_signer<S>(ws: WsConnect, signer: S) -> Result<WsProvider>
    where
        S: TxSigner<alloy::signers::Signature> + Send + Sync + 'static,
    {
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

        let fee = tx_receipt
            .gas_used
            .saturating_mul(tx_receipt.effective_gas_price);
        let blob_fee = Self::extract_blob_fee_from_receipt(&tx_receipt);

        Ok(Some(TransactionResponse::new(
            block_number,
            tx_receipt.status(),
            fee,
            blob_fee,
        )))
    }

    fn extract_block_number_from_receipt(receipt: &TransactionReceipt) -> Result<u64> {
        receipt.block_number.ok_or_else(|| {
            Error::Other("transaction receipt does not contain block number".to_string())
        })
    }

    fn extract_blob_fee_from_receipt(receipt: &TransactionReceipt) -> u128 {
        match (receipt.blob_gas_used, receipt.blob_gas_price) {
            (Some(gas_used), Some(gas_price)) => gas_used.saturating_mul(gas_price),
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {

    use alloy::{node_bindings::Anvil, signers::local::PrivateKeySigner};
    use services::{block_bundler::port::l1::FragmentEncoder, types::nonempty};

    use super::*;

    #[test]
    fn calculates_correctly_the_commit_height() {
        assert_eq!(
            WsConnection::calculate_commit_height(10, 3.try_into().unwrap()),
            U256::from(3)
        );
    }

    #[tokio::test]
    async fn submit_fragments_will_bump_gas_prices() {
        // given
        let anvil = Anvil::new()
            .args(["--hardfork", "cancun"])
            .try_spawn()
            .unwrap();

        let signer: PrivateKeySigner = anvil.keys()[0].clone().into();
        let blob_signer: PrivateKeySigner = anvil.keys()[1].clone().into();

        let wallet = EthereumWallet::from(signer.clone());
        let blob_wallet = EthereumWallet::from(blob_signer.clone());

        let ws = WsConnect::new(anvil.ws_endpoint());
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .wallet(wallet.clone())
            .on_ws(ws.clone())
            .await
            .unwrap();
        let blob_provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .wallet(blob_wallet.clone())
            .on_ws(ws)
            .await
            .unwrap();

        let connection = WsConnection {
            provider: provider.clone(),
            main_address: signer.address(),
            blob_provider: Some(blob_provider.clone()),
            blob_signer_address: Some(blob_signer.address()),
            contract: FuelStateContract::new(
                Address::from_slice([0u8; 20].as_ref()),
                provider.clone(),
            ),
            commit_interval: 3.try_into().unwrap(),
            tx_max_fee: u128::MAX,
            send_tx_request_timeout: Duration::from_secs(10),
            metrics: Default::default(),
        };

        let data = nonempty![1, 2, 3];
        let fragments = BlobEncoder.encode(data, 1.into()).unwrap();
        let sidecar = BlobEncoder::sidecar_from_fragments(fragments.clone()).unwrap();

        // create a tx with the help of the provider to get gas fields, hash etc
        let tx = TransactionRequest::default()
            .with_blob_sidecar(sidecar)
            .with_to(blob_signer.address());
        let tx = blob_provider.fill(tx).await.unwrap();
        let SendableTx::Envelope(tx) = tx else {
            panic!("Expected an envelope. This is a bug.");
        };
        let previous_tx = L1Tx {
            hash: tx.tx_hash().0,
            nonce: tx.nonce() as u32,
            max_fee: tx.max_fee_per_gas(),
            priority_fee: tx.max_priority_fee_per_gas().unwrap(),
            blob_fee: tx.max_fee_per_blob_gas().unwrap(),
            ..Default::default()
        };

        // when
        let (submitted_tx, _) = connection
            .submit_state_fragments(fragments, Some(previous_tx.clone()))
            .await
            .unwrap();

        // then
        assert_eq!(submitted_tx.nonce, previous_tx.nonce);
        assert_eq!(submitted_tx.max_fee, 2 * previous_tx.max_fee);
        assert_eq!(submitted_tx.priority_fee, 2 * previous_tx.priority_fee);
        assert_eq!(submitted_tx.blob_fee, 2 * previous_tx.blob_fee);
    }

    #[tokio::test]
    async fn submit_fragments_fails_if_max_fee_limit_exceeded() {
        // given
        let anvil = Anvil::new()
            .args(["--hardfork", "cancun"])
            .try_spawn()
            .unwrap();

        let signer: PrivateKeySigner = anvil.keys()[0].clone().into();
        let blob_signer: PrivateKeySigner = anvil.keys()[1].clone().into();

        let wallet = EthereumWallet::from(signer.clone());
        let blob_wallet = EthereumWallet::from(blob_signer.clone());

        let ws = WsConnect::new(anvil.ws_endpoint());
        let provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .wallet(wallet.clone())
            .on_ws(ws.clone())
            .await
            .unwrap();
        let blob_provider = ProviderBuilder::new()
            .with_recommended_fillers()
            .wallet(blob_wallet.clone())
            .on_ws(ws)
            .await
            .unwrap();

        let tx_max_fee = 1;
        let connection = WsConnection {
            provider: provider.clone(),
            main_address: signer.address(),
            blob_provider: Some(blob_provider.clone()),
            blob_signer_address: Some(blob_signer.address()),
            contract: FuelStateContract::new(
                Address::from_slice([0u8; 20].as_ref()),
                provider.clone(),
            ),
            commit_interval: 3.try_into().unwrap(),
            tx_max_fee,
            send_tx_request_timeout: Duration::from_secs(10),
            metrics: Default::default(),
        };

        let data = nonempty![1, 2, 3];
        let fragment = BlobEncoder.encode(data, 1.try_into().unwrap()).unwrap();

        // when
        let result = connection.submit_state_fragments(fragment, None).await;

        // then
        let result = result.expect_err("should return an error");
        assert!(result
            .to_string()
            .contains(&format!("limit {}", tx_max_fee)));
    }
}
