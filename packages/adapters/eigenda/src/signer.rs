use signers::{AwsKmsClient, KeySource};

use crate::error::ConnectorError;

pub struct SignedMessage {
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum EigenDASigner {
    AwsKms(AwsSigner),
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

    async fn sign_prehash(&self, message: &[u8]) -> Result<SignedMessage, ConnectorError> {
        // TODO unwrap
        let signed = self
            .client
            .sign_prehash(&self.key_id, message)
            .await
            .unwrap();

        Ok(SignedMessage { data: signed })
    }

    async fn account_id(&self) -> String {
        let pk = self.client.get_public_key(&self.key_id).await.unwrap();
        format!("0x{}", hex::encode(pk))
    }
}

// get the public key from the signing key
// let public_key = signing_key.verifying_key().to_encoded_point(false);
// let account_id = format!("0x{}", hex::encode(public_key.as_bytes()));

impl EigenDASigner {
    pub async fn new(key: KeySource) -> Self {
        match key {
            KeySource::Kms(key_id) => Self::AwsKms(AwsSigner::new(key_id).await),
            _ => unimplemented!(),
        }
    }

    pub async fn sign_prehash(&self, message: &[u8]) -> Result<SignedMessage, ConnectorError> {
        match self {
            Self::AwsKms(signer) => signer.sign_prehash(message).await,
        }
    }

    pub async fn account_id(&self) -> String {
        match self {
            Self::AwsKms(signer) => signer.account_id().await,
        }
    }
}
