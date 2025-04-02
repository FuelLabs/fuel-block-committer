use std::{
    cmp::min, num::NonZeroU32, ops::RangeInclusive, str::FromStr, sync::Arc, time::Duration,
};

use crate::{
    estimation::{MaxTxFeesPerGas, TransactionRequestExt},
    websocket::metrics::Metrics,
};
use ::metrics::RegistersMetrics;
use alloy::{
    consensus::{SignableTransaction, Transaction},
    eips::{
        BlockNumberOrTag,
        eip4844::DATA_GAS_PER_BLOB,
    },
    network::{Ethereum, EthereumWallet, TransactionBuilder, TransactionBuilder4844, TxSigner},
    primitives::{Address, B256, ChainId},
    providers::{
        Provider, ProviderBuilder, SendableTx, WsConnect,
        utils::{EIP1559_FEE_ESTIMATION_PAST_BLOCKS, Eip1559Estimation},
    },
    pubsub::PubSubFrontend,
    rpc::types::{FeeHistory, TransactionReceipt, TransactionRequest},
    signers::{Signature, local::PrivateKeySigner},
    sol,
};
use itertools::Itertools;
use serde::Deserialize;
use services::{
    state_committer::port::l1::Priority,
    types::{
        BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx, NonEmpty, TransactionResponse, U256,
    },
};
use tracing::info;
use url::Url;

use crate::{
    AwsClient, AwsConfig, Error, Result, blob_encoder,
    provider::L1Provider,
};

pub mod factory;
mod metrics;

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
pub struct WebsocketClient {
    blob_poster_address: Option<Address>,
    contract_caller_address: Address,
    provider: WsProvider,
    main_address: Address,
    blob_provider: Option<WsProvider>,
    blob_signer_address: Option<Address>,
    contract: FuelStateContract,
    commit_interval: NonZeroU32,
    metrics: Metrics,
    tx_config: TxConfig,
}

impl WebsocketClient {
    pub async fn connect(
        url: Url,
        contract_address: Address,
        signers: Signers,
        tx_config: TxConfig,
    ) -> Result<Self> {
        let blob_poster_address = signers
            .blob
            .as_ref()
            .map(|signer| TxSigner::address(&signer));
        let contract_caller_address = TxSigner::address(&signers.main);

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
            tx_config,
            metrics: Default::default(),
            blob_poster_address,
            contract_caller_address,
        })
    }

    fn set_metrics(&mut self, metrics: metrics::Metrics) {
        self.metrics = metrics;
    }

    #[cfg(feature = "test-helpers")]
    pub async fn finalized(&self, hash: [u8; 32], height: u32) -> Result<bool> {
        Ok(self
            .contract
            .finalized(hash.into(), U256::from(height))
            .call()
            .await?
            ._0)
    }

    pub fn blob_poster_address(&self) -> Option<Address> {
        self.blob_poster_address
    }

    pub fn contract_caller_address(&self) -> Address {
        self.contract_caller_address
    }

    async fn current_fees(&self, priority: Priority) -> Result<MaxTxFeesPerGas> {
        let priority_perc = self
            .tx_config
            .acceptable_priority_fee_percentage
            .apply(priority);

        let fee_history = self
            .provider
            .get_fee_history(
                EIP1559_FEE_ESTIMATION_PAST_BLOCKS,
                BlockNumberOrTag::Latest,
                &[priority_perc],
            )
            .await?;

        MaxTxFeesPerGas::try_from(fee_history)
    }

    fn get_max_fee(tx: &L1Tx, gas_limit: u128, num_fragments: usize) -> u128 {
        tx.max_fee.saturating_mul(gas_limit).saturating_add(
            tx.blob_fee
                .saturating_mul(num_fragments as u128)
                .saturating_mul(DATA_GAS_PER_BLOB as u128),
        )
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

impl L1Provider for WebsocketClient {
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
        let tx = tokio::time::timeout(self.tx_config.send_tx_request_timeout, send_fut)
            .await
            .map_err(|_| Error::Network {
                msg: "timed out trying to submit block".to_string(),
                recoverable: true,
            })??;
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

    async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
        reward_percentiles: &[f64],
    ) -> Result<FeeHistory> {
        let max = *height_range.end();
        let count = height_range.count() as u64;
        Ok(self
            .provider
            .get_fee_history(count, BlockNumberOrTag::Number(max), reward_percentiles)
            .await?)
    }

    async fn get_block_number(&self) -> Result<u64> {
        let response = self.provider.get_block_number().await?;
        Ok(response)
    }

    async fn balance(&self, address: Address) -> Result<U256> {
        Ok(self.provider.get_balance(address).await?)
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
        priority: Priority,
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
        let sidecar = blob_encoder::BlobEncoder::sidecar_from_fragments(limited_fragments)?;

        let projected_fees = self.current_fees(priority).await?.projected();

        let blob_tx = TransactionRequest::default()
            .with_blob_sidecar(sidecar)
            .with_to(*blob_signer_address);

        let blob_tx = if let Some(previous_tx) = previous_tx {
            let minimum_replacement_fees = MaxTxFeesPerGas::from(&previous_tx).double();
            let fees = projected_fees
                .retain_max(minimum_replacement_fees)
                .normalized();

            blob_tx
                .with_max_fees(fees)
                .with_nonce(previous_tx.nonce as u64)
        } else {
            blob_tx.with_max_fees(projected_fees.normalized())
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

        info!(
            "sending blob tx: {tx_id} with nonce: {}, max_fee_per_gas: {}, tip: {}, max_blob_fee_per_gas: {}",
            l1_tx.nonce, l1_tx.max_fee, l1_tx.priority_fee, l1_tx.blob_fee
        );

        let max_fee = WebsocketClient::get_max_fee(&l1_tx, blob_tx.gas_limit(), num_fragments);
        if max_fee > self.tx_config.tx_max_fee {
            return Err(Error::Other(
                format!(
                    "max fee exceeded: tried {}, limit {}",
                    max_fee, self.tx_config.tx_max_fee
                )
                .to_string(),
            ));
        }

        let send_fut = blob_provider.send_tx_envelope(blob_tx);
        let _ = tokio::time::timeout(self.tx_config.send_tx_request_timeout, send_fut)
            .await
            .map_err(|_| Error::Network {
                msg: "timed out trying to send blob tx".to_string(),
                recoverable: true,
            })??;

        self.metrics.blobs_per_tx.observe(num_fragments as f64);

        for bytes in unused_bytes_per_fragment {
            self.metrics.blob_unused_bytes.observe(bytes.into());
        }

        let fragments_submitted = FragmentsSubmitted {
            num_fragments: num_fragments.try_into().expect("cannot be zero"),
        };

        Ok((l1_tx, fragments_submitted))
    }

    fn commit_interval(&self) -> NonZeroU32 {
        self.commit_interval
    }

    fn blob_poster_address(&self) -> Option<Address> {
        self.blob_poster_address
    }

    fn contract_caller_address(&self) -> Address {
        self.contract_caller_address
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum L1Key {
    Kms(String),
    Private(String),
}

impl<'a> serde::Deserialize<'a> for L1Key {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        let value = String::deserialize(deserializer)?;
        if let Some(k) = value.strip_prefix("Kms(").and_then(|s| s.strip_suffix(')')) {
            Ok(L1Key::Kms(k.to_string()))
        } else if let Some(k) = value
            .strip_prefix("Private(")
            .and_then(|s| s.strip_suffix(')'))
        {
            Ok(L1Key::Private(k.to_string()))
        } else {
            Err(serde::de::Error::custom("invalid L1Key format"))
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct L1Keys {
    /// The eth key authorized by the L1 bridging contracts to post block commitments.
    pub main: L1Key,
    /// The eth key for posting L2 state to L1.
    pub blob: Option<L1Key>,
}

impl L1Keys {
    pub fn uses_aws(&self) -> bool {
        matches!(self.main, L1Key::Kms(_)) || matches!(self.blob, Some(L1Key::Kms(_)))
    }
}

#[derive(Debug, Clone)]
pub struct TxConfig {
    pub tx_max_fee: u128,
    pub send_tx_request_timeout: Duration,
    pub acceptable_priority_fee_percentage: AcceptablePriorityFeePercentages,
}

#[cfg(feature = "test-helpers")]
impl Default for TxConfig {
    fn default() -> Self {
        Self {
            tx_max_fee: u128::MAX,
            send_tx_request_timeout: Duration::from_secs(10),
            acceptable_priority_fee_percentage: AcceptablePriorityFeePercentages::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AcceptablePriorityFeePercentages {
    min: f64,
    max: f64,
}

#[cfg(feature = "test-helpers")]
impl Default for AcceptablePriorityFeePercentages {
    fn default() -> Self {
        Self::new(20., 20.).expect("valid reward percentile range")
    }
}

impl AcceptablePriorityFeePercentages {
    pub fn new(min: f64, max: f64) -> Result<Self> {
        if min > max {
            return Err(crate::Error::Other(
                "min reward percentile must be less than or equal to max reward percentile"
                    .to_string(),
            ));
        }

        if min <= 0.0 || max > 100.0 {
            return Err(crate::Error::Other(
                "reward percentiles must be > 0 and <= 100".to_string(),
            ));
        }

        Ok(Self { min, max })
    }

    pub fn apply(&self, priority: Priority) -> f64 {
        let min = self.min;

        let increase = (self.max - min) * priority.get() / 100.;

        (min + increase).min(self.max)
    }
}

// This trait is needed because you cannot write `dyn TraitA + TraitB` except when TraitB is an
// auto-trait.
trait CompositeSigner: alloy::signers::Signer + TxSigner<Signature> {}
impl<T: alloy::signers::Signer + TxSigner<Signature>> CompositeSigner for T {}

#[derive(Clone)]
pub struct Signer {
    signer: Arc<dyn CompositeSigner + 'static + Send + Sync>,
    chain_id: Option<ChainId>,
}

#[async_trait::async_trait]
impl TxSigner<Signature> for Signer {
    fn address(&self) -> Address {
        TxSigner::<Signature>::address(&self.signer)
    }

    async fn sign_transaction(
        &self,
        tx: &mut dyn SignableTransaction<Signature>,
    ) -> alloy::signers::Result<Signature> {
        TxSigner::<Signature>::sign_transaction(&self.signer, tx).await
    }
}

#[async_trait::async_trait]
impl alloy::signers::Signer<Signature> for Signer {
    async fn sign_hash(&self, hash: &B256) -> alloy::signers::Result<Signature> {
        self.signer.sign_hash(hash).await
    }

    fn address(&self) -> Address {
        alloy::signers::Signer::<Signature>::address(&*self.signer)
    }

    fn chain_id(&self) -> Option<ChainId> {
        self.chain_id
    }

    fn set_chain_id(&mut self, chain_id: Option<ChainId>) {
        self.chain_id = chain_id;
    }
}

impl Signer {
    pub async fn make_aws_signer(client: &AwsClient, key: String) -> Result<Self> {
        let signer = client.make_signer(key).await?;
        let chain_id = alloy::signers::Signer::chain_id(&signer);

        Ok(Signer {
            signer: Arc::new(signer),
            chain_id,
        })
    }

    pub fn make_private_key_signer(key: &str) -> Result<Self> {
        let signer = PrivateKeySigner::from_str(key)
            .map_err(|_| Error::Other("Invalid private key".to_string()))?;
        let chain_id = signer.chain_id();

        Ok(Signer {
            signer: Arc::new(signer),
            chain_id,
        })
    }
}

pub struct Signers {
    pub main: Signer,
    pub blob: Option<Signer>,
}

impl Clone for Signers {
    fn clone(&self) -> Self {
        Self {
            main: self.main.clone(),
            blob: self.blob.clone(),
        }
    }
}

impl Signers {
    pub async fn for_keys(keys: L1Keys) -> Result<Self> {
        let aws_client = if keys.uses_aws() {
            let config = AwsConfig::from_env().await;
            Some(AwsClient::new(config))
        } else {
            None
        };

        let blob_signer = match keys.blob {
            Some(L1Key::Kms(key)) => {
                Some(Signer::make_aws_signer(aws_client.as_ref().expect("is set"), key).await?)
            }
            Some(L1Key::Private(key)) => Some(Signer::make_private_key_signer(&key)?),
            None => None,
        };

        let main_signer = match keys.main {
            L1Key::Kms(key) => Signer::make_aws_signer(&aws_client.expect("is set"), key).await?,
            L1Key::Private(key) => Signer::make_private_key_signer(&key)?,
        };

        Ok(Self {
            main: main_signer,
            blob: blob_signer,
        })
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use services::state_committer::port::l1::Priority;

    use super::L1Key;

    #[test]
    fn can_deserialize_private_key() {
        // given
        let val = r#""Private(0x1234)""#;

        // when
        let key: L1Key = serde_json::from_str(val).unwrap();

        // then
        assert_eq!(key, L1Key::Private("0x1234".to_owned()));
    }

    #[test]
    fn can_deserialize_kms_key() {
        // given
        let val = r#""Kms(0x1234)""#;

        // when
        let key: L1Key = serde_json::from_str(val).unwrap();

        // then
        assert_eq!(key, L1Key::Kms("0x1234".to_owned()));
    }

    #[test]
    fn lowest_priority_gives_min_priority_fee_perc() {
        // given
        let sut = super::AcceptablePriorityFeePercentages::new(20., 40.).unwrap();

        // when
        let fee_perc = sut.apply(Priority::MIN);

        // then
        assert_eq!(fee_perc, 20.);
    }

    #[test]
    fn medium_priority_gives_middle_priority_fee_perc() {
        // given
        let sut = super::AcceptablePriorityFeePercentages::new(20., 40.).unwrap();

        // when
        let fee_perc = sut.apply(Priority::new(50.).unwrap());

        // then
        assert_eq!(fee_perc, 30.);
    }

    #[test]
    fn highest_priority_gives_max_priority_fee_perc() {
        // given
        let sut = super::AcceptablePriorityFeePercentages::new(20., 40.).unwrap();

        // when
        let fee_perc = sut.apply(Priority::MAX);

        // then
        assert_eq!(fee_perc, 40.);
    }

    use alloy::{node_bindings::Anvil, signers::local::PrivateKeySigner};
    use services::{block_bundler::port::l1::FragmentEncoder, types::nonempty};

    use super::*;
    use crate::blob_encoder;

    #[test]
    fn calculates_correctly_the_commit_height() {
        assert_eq!(
            WebsocketClient::calculate_commit_height(10, 3.try_into().unwrap()),
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

        let connection = WebsocketClient {
            blob_poster_address: None,
            contract_caller_address: Address::ZERO,
            provider: provider.clone(),
            main_address: signer.address(),
            blob_provider: Some(blob_provider.clone()),
            blob_signer_address: Some(blob_signer.address()),
            contract: FuelStateContract::new(
                Address::from_slice([0u8; 20].as_ref()),
                provider.clone(),
            ),
            commit_interval: 3.try_into().unwrap(),
            tx_config: TxConfig::default(),
            metrics: Default::default(),
        };

        let data = nonempty![1, 2, 3];
        let fragments = blob_encoder::BlobEncoder.encode(data, 1.into()).unwrap();
        let sidecar = blob_encoder::BlobEncoder::sidecar_from_fragments(fragments.clone()).unwrap();

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
            .submit_state_fragments(fragments, Some(previous_tx.clone()), Priority::MIN)
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
        let connection = WebsocketClient {
            blob_poster_address: None,
            contract_caller_address: Address::ZERO,
            provider: provider.clone(),
            main_address: signer.address(),
            blob_provider: Some(blob_provider.clone()),
            blob_signer_address: Some(blob_signer.address()),
            contract: FuelStateContract::new(
                Address::from_slice([0u8; 20].as_ref()),
                provider.clone(),
            ),
            commit_interval: 3.try_into().unwrap(),
            tx_config: TxConfig {
                tx_max_fee,
                ..Default::default()
            },
            metrics: Default::default(),
        };

        let data = nonempty![1, 2, 3];
        let fragment = blob_encoder::BlobEncoder
            .encode(data, 1.try_into().unwrap())
            .unwrap();

        // when
        let result = connection
            .submit_state_fragments(fragment, None, Priority::MIN)
            .await;

        // then
        let result = result.expect_err("should return an error");
        assert!(
            result
                .to_string()
                .contains(&format!("limit {}", tx_max_fee))
        );
    }
}
