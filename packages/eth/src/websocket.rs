use std::{num::NonZeroU32, str::FromStr, time::Duration};

use ::metrics::{prometheus::core::Collector, HealthChecker, RegistersMetrics};
use alloy::{
    consensus::SignableTransaction,
    network::TxSigner,
    primitives::{Address, ChainId, Sign, B256},
    signers::{k256::SecretKey, local::PrivateKeySigner, Signature, Signer},
};
use ports::{
    l1::{FragmentsSubmitted, Result},
    types::{BlockSubmissionTx, Fragment, L1Tx, NonEmpty, TransactionResponse, U256},
};
use serde::Deserialize;
use url::Url;

use self::{
    connection::WsConnection,
    health_tracking_middleware::{EthApi, HealthTrackingMiddleware},
};
use crate::{AwsClient, AwsConfig};

mod connection;
mod health_tracking_middleware;

#[derive(Clone)]
pub struct WebsocketClient {
    inner: HealthTrackingMiddleware<WsConnection>,
    blob_poster_address: Option<Address>,
    contract_caller_address: Address,
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
}

trait OurTrait: Signer + TxSigner<Signature> {}
impl<T: Signer + TxSigner<Signature>> OurTrait for T {}

pub struct OurSigner {
    signer: Box<dyn OurTrait + 'static + Send + Sync>,
}

#[async_trait::async_trait]
impl TxSigner<Signature> for OurSigner {
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
impl Signer<Signature> for OurSigner {
    /// Signs the given hash.
    async fn sign_hash(&self, hash: &B256) -> alloy::signers::Result<Signature> {
        self.signer.sign_hash(hash).await
    }

    fn address(&self) -> Address {
        Signer::<Signature>::address(&self.signer)
    }

    fn chain_id(&self) -> Option<ChainId> {
        self.signer.chain_id()
    }

    fn set_chain_id(&mut self, chain_id: Option<ChainId>) {
        self.signer.set_chain_id(chain_id)
    }
}

impl OurSigner {
    pub async fn make_aws_signer(client: &AwsClient, key: String) -> Self {
        let signer = client.make_signer(key).await.unwrap();

        OurSigner {
            signer: Box::new(signer),
        }
    }

    pub fn make_private_key_signer(key: &str) -> Self {
        let signer = PrivateKeySigner::from_str(&key).unwrap();
        OurSigner {
            signer: Box::new(signer),
        }
    }
}

pub struct OurSigners {
    pub main: OurSigner,
    pub blob: Option<OurSigner>,
}
impl OurSigners {
    pub async fn for_keys(keys: L1Keys) -> Self {
        let aws_client = if keys.uses_aws() {
            let config = AwsConfig::from_env().await;
            Some(AwsClient::new(config))
        } else {
            None
        };

        let blob_signer = if let Some(blob_key) = keys.blob {
            let signer = match blob_key {
                L1Key::Kms(key) => {
                    OurSigner::make_aws_signer(aws_client.as_ref().expect("is set"), key).await
                }
                L1Key::Private(key) => OurSigner::make_private_key_signer(&key),
            };
            Some(signer)
        } else {
            None
        };

        let l1_key = keys.main;
        let main_signer = match l1_key {
            L1Key::Kms(key) => OurSigner::make_aws_signer(&aws_client.expect("is set"), key).await,
            L1Key::Private(key) => OurSigner::make_private_key_signer(&key),
        };

        Self {
            main: main_signer,
            blob: blob_signer,
        }
    }
}

impl WebsocketClient {
    pub async fn connect(
        url: Url,
        contract_address: Address,
        signers: OurSigners,
        unhealthy_after_n_errors: usize,
        tx_config: TxConfig,
    ) -> ports::l1::Result<Self> {
        let blob_poster_address = signers
            .blob
            .as_ref()
            .map(|signer| TxSigner::address(&signer));
        let contract_caller_address = TxSigner::address(&signers.main);

        let provider = WsConnection::connect(
            url,
            contract_address,
            signers,
            tx_config.tx_max_fee,
            tx_config.send_tx_request_timeout,
        )
        .await?;

        Ok(Self {
            inner: HealthTrackingMiddleware::new(provider, unhealthy_after_n_errors),
            blob_poster_address,
            contract_caller_address,
        })
    }

    #[must_use]
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

    pub(crate) async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> Result<bool> {
        Ok(self.inner.is_squeezed_out(tx_hash).await?)
    }

    pub(crate) async fn balance(&self, address: Address) -> Result<U256> {
        Ok(self.inner.balance(address).await?)
    }

    pub(crate) async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<ports::types::L1Tx>,
    ) -> ports::l1::Result<(L1Tx, FragmentsSubmitted)> {
        Ok(self
            .inner
            .submit_state_fragments(fragments, previous_tx)
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
