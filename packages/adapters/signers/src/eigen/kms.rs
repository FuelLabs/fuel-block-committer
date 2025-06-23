use async_trait::async_trait;
use aws_sdk_kms::{Client as AwsKmsClient, primitives::Blob};
use k256::{
    ecdsa::{RecoveryId, Signature, VerifyingKey},
    pkcs8::DecodePublicKey,
};
use rust_eigenda_signers::{Message, RecoverableSignature};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("KMS GetPublicKey operation failed")]
    KmsGetPublicKey(#[from] aws_sdk_kms::operation::get_public_key::GetPublicKeyError),

    #[error("KMS Sign operation failed")]
    KmsSign(#[from] aws_sdk_kms::operation::sign::SignError),

    #[error("Failed to get public key from KMS")]
    MissingPublicKey,

    #[error("Failed to get signature from KMS")]
    MissingSignature,

    #[error("Failed to parse DER-encoded public key")]
    InvalidPublicKeyDer(#[source] k256::pkcs8::spki::Error),

    #[error("Failed to parse DER-encoded signature")]
    InvalidSignatureDer(#[source] k256::ecdsa::Error),

    #[error("Failed to convert public key format")]
    PublicKeyConversion(#[source] rust_eigenda_signers::PublicKeyError),

    #[error("Invalid public key length: expected 65 bytes, got {0}")]
    InvalidPublicKeyLength(usize),

    #[error("Invalid recoverable signature")]
    InvalidRecoverableSignature(#[source] rust_eigenda_signers::InvalidRecoveryId),

    #[error("Failed to parse SEC1-encoded public key")]
    InvalidSec1PublicKey(#[source] k256::elliptic_curve::Error),

    #[error("Could not determine recovery ID for signature")]
    RecoveryIdNotFound,

    #[error("Invalid recovery ID: {0}")]
    InvalidRecoveryId(u8),
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Signer {
    key_id: String,
    client: AwsKmsClient,
    public_key: rust_eigenda_signers::PublicKey,
    k256_verifying_key: VerifyingKey,
}

impl Signer {
    pub async fn new(client: AwsKmsClient, key_id: String) -> Result<Self> {
        let response = client
            .get_public_key()
            .key_id(&key_id)
            .send()
            .await
            .map_err(|e| e.into_service_error())?;

        let public_key_der = response
            .public_key
            .ok_or(Error::MissingPublicKey)?
            .into_inner();

        // Parse the DER-encoded public key using k256
        let k256_verifying_key = VerifyingKey::from_public_key_der(&public_key_der)
            .map_err(Error::InvalidPublicKeyDer)?;

        // Convert k256 public key to rust_eigenda_signers public key
        // Use uncompressed format directly
        let uncompressed_bytes = k256_verifying_key.to_encoded_point(false);
        let uncompressed_slice = uncompressed_bytes.as_bytes();
        let uncompressed_array: [u8; 65] = uncompressed_slice
            .try_into()
            .map_err(|_| Error::InvalidPublicKeyLength(uncompressed_slice.len()))?;

        let public_key = rust_eigenda_signers::PublicKey::new(uncompressed_array)
            .map_err(Error::PublicKeyConversion)?;

        Ok(Self {
            key_id,
            client,
            public_key,
            k256_verifying_key,
        })
    }

    pub fn inner(&self) -> &AwsKmsClient {
        &self.client
    }

    pub async fn get_public_key(&self, key_id: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get_public_key()
            .key_id(key_id)
            .send()
            .await
            .map_err(|e| e.into_service_error())?;

        let der_bytes = response
            .public_key
            .ok_or(Error::MissingPublicKey)?
            .into_inner();

        // Convert to uncompressed SEC1 format
        let verifying_key =
            VerifyingKey::from_public_key_der(&der_bytes).map_err(Error::InvalidPublicKeyDer)?;

        Ok(verifying_key.to_encoded_point(false).as_bytes().to_vec())
    }

    async fn sign_with_kms(&self, key_id: &str, digest: &[u8]) -> Result<Signature> {
        let response = self
            .client
            .sign()
            .key_id(key_id)
            .message(Blob::new(digest))
            .message_type(aws_sdk_kms::types::MessageType::Digest)
            .signing_algorithm(aws_sdk_kms::types::SigningAlgorithmSpec::EcdsaSha256)
            .send()
            .await
            .map_err(|e| e.into_service_error())?;

        let signature_der = response.signature.ok_or(Error::MissingSignature)?;

        let signature =
            Signature::from_der(signature_der.as_ref()).map_err(Error::InvalidSignatureDer)?;

        // Normalize S value to ensure it's in the lower half of the order
        Ok(signature.normalize_s().unwrap_or(signature))
    }

    #[allow(clippy::result_large_err)]
    fn compute_recovery_id(&self, digest: &[u8; 32], signature: &Signature) -> Result<u8> {
        // For secp256k1, only recovery IDs 0 and 1 are valid for uncompressed keys
        for recovery_id in 0..2 {
            let rec_id =
                RecoveryId::from_byte(recovery_id).ok_or(Error::InvalidRecoveryId(recovery_id))?;

            if let Ok(recovered_key) = VerifyingKey::recover_from_prehash(digest, signature, rec_id)
            {
                if recovered_key == self.k256_verifying_key {
                    return Ok(recovery_id);
                }
            }
        }

        Err(Error::RecoveryIdNotFound)
    }
}

#[async_trait]
impl eigenda::Sign for Signer {
    type Error = Error;

    async fn sign_digest(&self, message: &Message) -> Result<RecoverableSignature> {
        let digest_bytes: &[u8; 32] = message;

        // Sign the digest with KMS
        let signature = self.sign_with_kms(&self.key_id, digest_bytes).await?;

        // Compute recovery ID
        let recovery_id = self.compute_recovery_id(digest_bytes, &signature)?;

        // Build recoverable signature
        let mut sig_bytes = [0u8; 65];
        sig_bytes[..64].copy_from_slice(&signature.to_bytes());
        sig_bytes[64] = recovery_id;

        RecoverableSignature::from_bytes(&sig_bytes).map_err(Error::InvalidRecoverableSignature)
    }

    fn public_key(&self) -> rust_eigenda_signers::PublicKey {
        self.public_key
    }
}
