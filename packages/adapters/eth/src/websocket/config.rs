use crate::AwsConfig;
use crate::Error;
use crate::Result;

use alloy::signers::local::PrivateKeySigner;

use crate::AwsClient;

use alloy::primitives::B256;

use alloy::consensus::SignableTransaction;

use alloy::primitives::Address;

use alloy::primitives::ChainId;

use std::str::FromStr;
use std::sync::Arc;

use alloy::signers::Signature;

use alloy::network::TxSigner;

use services::state_committer::port::l1::Priority;

use std::time::Duration;

use serde::Deserialize;

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
    pub(crate) min: f64,
    pub(crate) max: f64,
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
pub(crate) trait CompositeSigner: alloy::signers::Signer + TxSigner<Signature> {}

impl<T: alloy::signers::Signer + TxSigner<Signature>> CompositeSigner for T {}

#[derive(Clone)]
pub struct Signer {
    pub(crate) signer: Arc<dyn CompositeSigner + 'static + Send + Sync>,
    pub(crate) chain_id: Option<ChainId>,
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
