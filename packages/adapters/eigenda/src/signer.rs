use k256::{ecdsa::SigningKey, SecretKey};
use sha3::{Digest, Keccak256};
use signers::{AwsKmsClient, KeySource};

use crate::error::{Error, Result};

type SignedMessage = Vec<u8>;

#[derive(Debug, Clone)]
pub enum EigenDASigner {
    AwsKms(AwsSigner),
    PrivateKey(PrivateKeySigner),
}

#[derive(Debug, Clone)]
pub struct AwsSigner {
    key_id: String,
    client: AwsKmsClient,
}

impl AwsSigner {
    async fn new(key_id: String) -> Self {
        let client = AwsKmsClient::new().await;

        Self { key_id, client }
    }

    async fn sign_prehash(&self, message: &[u8]) -> Result<SignedMessage> {
        let signed = self
            .client
            .sign_prehash(&self.key_id, message)
            .await
            .map_err(|e| Error::Other(format!("failed to sign message: {}", e.to_string())))?;

        Ok(signed)
    }

    async fn account_id(&self) -> String {
        let pk = self.client.get_public_key(&self.key_id).await.unwrap();
        format!("0x{}", hex::encode(pk))
    }
}

#[derive(Debug, Clone)]
pub struct PrivateKeySigner {
    signing_key: SigningKey,
}

impl PrivateKeySigner {
    pub fn new(key: String) -> Result<Self> {
        let key_bytes = hex::decode(key)
            .map_err(|_| Error::Other("failed to decode private key".to_string()))?;
        let secret_key = SecretKey::from_slice(&key_bytes)
            .map_err(|_| Error::Other("failed to create secret key".to_string()))?;
        let signing_key = SigningKey::from(secret_key);

        Ok(Self { signing_key })
    }

    pub fn sign_prehash(&self, message: &[u8]) -> Result<SignedMessage> {
        let (sig, recid) = self
            .signing_key
            .sign_prehash_recoverable(&message)
            .map_err(|e| Error::Other(format!("message signing failed: {}", e.to_string())))?;

        let mut signature = sig.to_vec();
        signature.push(recid.to_byte());

        Ok(signature)
    }

    pub fn account_id(&self) -> String {
        let public_key = self.signing_key.verifying_key().to_encoded_point(false);
        let account_id = format!("0x{}", hex::encode(public_key.as_bytes()));

        account_id
    }
}

impl EigenDASigner {
    pub async fn new(key: KeySource) -> Result<Self> {
        match key {
            KeySource::Kms(key_id) => {
                let signer = AwsSigner::new(key_id).await;
                Ok(Self::AwsKms(signer))
            }
            KeySource::Private(key) => {
                let signer = PrivateKeySigner::new(key)?;
                Ok(Self::PrivateKey(signer))
            }
        }
    }

    pub async fn sign_prehash(&self, message: &[u8]) -> Result<SignedMessage> {
        match self {
            Self::AwsKms(signer) => signer.sign_prehash(message).await,
            Self::PrivateKey(signer) => signer.sign_prehash(message),
        }
    }

    pub async fn account_id(&self) -> String {
        match self {
            Self::AwsKms(signer) => signer.account_id().await,
            Self::PrivateKey(signer) => signer.account_id(),
        }
    }
}
