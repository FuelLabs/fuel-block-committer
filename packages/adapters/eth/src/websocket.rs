use std::{num::NonZeroU32, ops::RangeInclusive, str::FromStr, time::Duration};

use ::metrics::{prometheus::core::Collector, HealthChecker, RegistersMetrics};
use alloy::{
    consensus::SignableTransaction,
    network::TxSigner,
    primitives::{Address, ChainId, B256},
    rpc::types::FeeHistory,
    signers::{aws::AwsSigner, local::PrivateKeySigner, Signature},
};
use delegate::delegate;
use serde::Deserialize;
use services::{
    state_committer::port::da_layer::Priority,
    types::{
        BlockSubmissionTx, EthereumDASubmission, Fragment, FragmentsSubmitted, L1Height, NonEmpty,
        TransactionResponse, U256,
    },
    Result,
};
use signers::{AwsKmsClient, KeySource};
use url::Url;

use self::{
    connection::WsConnection,
    health_tracking_middleware::{EthApi, HealthTrackingMiddleware},
};
use crate::{error::Error, fee_api_helpers::batch_requests};

mod connection;
mod health_tracking_middleware;

#[derive(Clone)]
pub struct WebsocketClient {
    inner: HealthTrackingMiddleware<WsConnection>,
    blob_poster_address: Option<Address>,
    contract_caller_address: Address,
}

impl services::block_committer::port::l1::Contract for WebsocketClient {
    delegate! {
        to self {
            async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx>;
            fn commit_interval(&self) -> NonZeroU32;
        }
    }
}

impl services::state_listener::port::l1::Api for WebsocketClient {
    delegate! {
        to (*self) {
            async fn get_transaction_response(&self, tx_hash: [u8; 32],) -> Result<Option<TransactionResponse>>;
            async fn is_squeezed_out(&self, tx_hash: [u8; 32],) -> Result<bool>;
        }
    }

    async fn get_block_number(&self) -> Result<L1Height> {
        let block_num = self._get_block_number().await?;
        let height = L1Height::try_from(block_num)?;

        Ok(height)
    }
}

impl services::wallet_balance_tracker::port::l1::Api for WebsocketClient {
    delegate! {
        to (*self) {
            async fn balance(&self, address: Address) -> Result<U256>;
        }
    }
}

impl services::block_committer::port::l1::Api for WebsocketClient {
    delegate! {
        to (*self) {
            async fn get_transaction_response(&self, tx_hash: [u8; 32],) -> Result<Option<TransactionResponse>>;
        }
    }

    async fn get_block_number(&self) -> Result<L1Height> {
        let block_num = self._get_block_number().await?;
        let height = L1Height::try_from(block_num)?;

        Ok(height)
    }
}

impl services::fees::Api for WebsocketClient {
    async fn current_height(&self) -> Result<u64> {
        self._get_block_number().await
    }

    async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
    ) -> Result<services::fees::SequentialBlockFees> {
        batch_requests(height_range, move |sub_range, percentiles| async move {
            self.fees(sub_range, percentiles).await
        })
        .await
    }
}

impl services::state_committer::port::da_layer::Api for WebsocketClient {
    async fn current_height(&self) -> Result<u64> {
        self._get_block_number().await
    }

    delegate! {
        to (*self) {
            async fn submit_state_fragments(
                &self,
                fragments: NonEmpty<Fragment>,
                previous_tx: Option<services::types::EthereumDASubmission>,
                priority: Priority
            ) -> Result<(EthereumDASubmission, FragmentsSubmitted)>;
        }
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
    pub main: KeySource,
    /// The eth key for posting L2 state to L1.
    pub blob: Option<KeySource>,
}

impl L1Keys {
    pub fn uses_aws(&self) -> bool {
        matches!(self.main, KeySource::Kms(_)) || matches!(self.blob, Some(KeySource::Kms(_)))
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
            return Err(services::Error::Other(
                "min reward percentile must be less than or equal to max reward percentile"
                    .to_string(),
            ));
        }

        if min <= 0.0 || max > 100.0 {
            return Err(services::Error::Other(
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

pub struct Signer {
    signer: Box<dyn CompositeSigner + 'static + Send + Sync>,
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
        alloy::signers::Signer::<Signature>::address(&self.signer)
    }

    fn chain_id(&self) -> Option<ChainId> {
        self.signer.chain_id()
    }

    fn set_chain_id(&mut self, chain_id: Option<ChainId>) {
        self.signer.set_chain_id(chain_id)
    }
}

impl Signer {
    pub async fn make_aws_signer(client: &AwsKmsClient, key: String) -> Result<Self> {
        let signer = AwsSigner::new(client.inner().clone(), key, None)
            .await
            .map_err(|err| Error::Other(format!("Error making aws signer: {err:?}")))?;

        Ok(Signer {
            signer: Box::new(signer),
        })
    }

    pub fn make_private_key_signer(key: &str) -> Result<Self> {
        let signer = PrivateKeySigner::from_str(key)
            .map_err(|_| services::Error::Other("Invalid private key".to_string()))?;

        Ok(Signer {
            signer: Box::new(signer),
        })
    }
}

pub struct Signers {
    pub main: Signer,
    pub blob: Option<Signer>,
}
impl Signers {
    pub async fn for_keys(keys: L1Keys) -> Result<Self> {
        let aws_client = if keys.uses_aws() {
            let client = AwsKmsClient::new().await;
            Some(client)
        } else {
            None
        };

        let blob_signer = match keys.blob {
            Some(KeySource::Kms(key)) => {
                Some(Signer::make_aws_signer(aws_client.as_ref().expect("is set"), key).await?)
            }
            Some(KeySource::Private(key)) => Some(Signer::make_private_key_signer(&key)?),
            None => None,
        };

        let main_signer = match keys.main {
            KeySource::Kms(key) => {
                Signer::make_aws_signer(&aws_client.expect("is set"), key).await?
            }
            KeySource::Private(key) => Signer::make_private_key_signer(&key)?,
        };

        Ok(Self {
            main: main_signer,
            blob: blob_signer,
        })
    }
}

impl WebsocketClient {
    pub async fn connect(
        url: Url,
        contract_address: Address,
        signers: Signers,
        unhealthy_after_n_errors: usize,
        tx_config: TxConfig,
    ) -> services::Result<Self> {
        let blob_poster_address = signers
            .blob
            .as_ref()
            .map(|signer| TxSigner::address(&signer));
        let contract_caller_address = TxSigner::address(&signers.main);

        let provider = WsConnection::connect(url, contract_address, signers, tx_config).await?;

        Ok(Self {
            inner: HealthTrackingMiddleware::new(provider, unhealthy_after_n_errors),
            blob_poster_address,
            contract_caller_address,
        })
    }

    pub fn connection_health_checker(&self) -> HealthChecker {
        self.inner.connection_health_checker()
    }

    pub(crate) async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx> {
        Ok(self.inner.submit(hash, height).await?)
    }

    pub(crate) fn commit_interval(&self) -> NonZeroU32 {
        self.inner.commit_interval()
    }

    pub(crate) async fn _get_block_number(&self) -> Result<u64> {
        Ok(self.inner.get_block_number().await?)
    }

    pub(crate) async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>> {
        Ok(self.inner.get_transaction_response(tx_hash).await?)
    }

    pub(crate) async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
        rewards_percentile: &[f64],
    ) -> Result<FeeHistory> {
        Ok(self.inner.fees(height_range, rewards_percentile).await?)
    }

    pub(crate) async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> Result<bool> {
        Ok(self.inner.is_squeezed_out(tx_hash).await?)
    }

    pub(crate) async fn balance(&self, address: Address) -> Result<U256> {
        Ok(self.inner.balance(address).await?)
    }

    pub(crate) async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<services::types::EthereumDASubmission>,
        priority: Priority,
    ) -> Result<(EthereumDASubmission, FragmentsSubmitted)> {
        Ok(self
            .inner
            .submit_state_fragments(fragments, previous_tx, priority)
            .await?)
    }

    #[cfg(feature = "test-helpers")]
    pub async fn finalized(&self, hash: [u8; 32], height: u32) -> Result<bool> {
        Ok(self.inner.finalized(hash, height).await?)
    }

    #[cfg(feature = "test-helpers")]
    pub async fn block_hash_at_commit_height(&self, commit_height: u32) -> Result<[u8; 32]> {
        Ok(self
            .inner
            .block_hash_at_commit_height(commit_height)
            .await?)
    }

    pub fn blob_poster_address(&self) -> Option<Address> {
        self.blob_poster_address
    }

    pub fn contract_caller_address(&self) -> Address {
        self.contract_caller_address
    }
}

// User responsible for registering any metrics T might have
impl RegistersMetrics for WebsocketClient {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        self.inner.metrics()
    }
}
