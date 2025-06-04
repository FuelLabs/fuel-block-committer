use anyhow::Context;
use async_trait::async_trait;
use aws_sdk_kms::Client as InnerClient;
use aws_sdk_kms::primitives::Blob;
use k256::{
    ecdsa::{RecoveryId, Signature, VerifyingKey},
    pkcs8::DecodePublicKey,
};
use rust_eigenda_signers::{Message, RecoverableSignature};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct Signer {
    key_id: String,
    client: InnerClient,
    public_key: rust_eigenda_signers::PublicKey,
}

impl Signer {
    pub async fn new(client: InnerClient, key_id: String) -> anyhow::Result<Self> {
        let public_key_der = client
            .get_public_key()
            .key_id(&key_id)
            .send()
            .await
            .context("Failed to get public key")?
            .public_key
            .context("Public key missing from response")?
            .into_inner();

        // Parse the DER-encoded public key using k256
        let k256_pub_key = VerifyingKey::from_public_key_der(&public_key_der)
            .context("Failed to parse public key DER from KMS")?;

        // Convert k256 public key to secp256k1 public key
        let mut pub_key: [u8; 65] = [0; 65];
        pub_key.copy_from_slice(k256_pub_key.to_encoded_point(false).as_bytes());
        let secp_pub_key = rust_eigenda_signers::PublicKey::new(pub_key)
            .context("Failed to convert k256 pubkey to secp256k1 pubkey")?;

        Ok(Self {
            key_id,
            client,
            public_key: secp_pub_key,
        })
    }

    pub fn inner(&self) -> &InnerClient {
        &self.client
    }

    async fn get_raw_signature(&self, key_id: &str, data: &[u8]) -> anyhow::Result<Signature> {
        let response = self
            .client
            .sign()
            .key_id(key_id)
            .message(data.into())
            .message_type(aws_sdk_kms::types::MessageType::Digest)
            .signing_algorithm(aws_sdk_kms::types::SigningAlgorithmSpec::EcdsaSha256)
            .send()
            .await?;

        let der_signature = response
            .signature
            .ok_or_else(|| anyhow::anyhow!("kms signature missing"))?;

        let signature = Signature::from_der(der_signature.as_ref())?;
        Ok(signature.normalize_s().unwrap_or(signature))
    }

    fn determine_recovery_id(
        &self,
        prehash: &[u8],
        signature: &Signature,
        public_key: &[u8],
    ) -> anyhow::Result<u8> {
        let expected_key = VerifyingKey::from_sec1_bytes(public_key)?;

        // try both possible recovery IDs
        for recovery_id in 0..2 {
            if let Ok(recovered_key) = VerifyingKey::recover_from_prehash(
                prehash,
                signature,
                RecoveryId::from_byte(recovery_id).expect("valid recovery id"),
            ) {
                if recovered_key == expected_key {
                    return Ok(recovery_id);
                }
            }
        }

        Err(anyhow::anyhow!("Could not determine recovery ID"))
    }

    pub async fn sign_prehash(&self, key_id: &str, prehash: &[u8]) -> anyhow::Result<Vec<u8>> {
        let public_key = self.get_public_key(key_id).await?;
        let signature = self.get_raw_signature(key_id, prehash).await?;

        let recovery_id = self.determine_recovery_id(prehash, &signature, &public_key)?;

        // combine into final signature
        let mut signature_bytes = signature.to_bytes().to_vec();
        signature_bytes.push(recovery_id);

        Ok(signature_bytes)
    }

    pub async fn get_public_key(&self, key_id: &str) -> anyhow::Result<Vec<u8>> {
        let key_info = self.client.get_public_key().key_id(key_id).send().await?;

        let der_bytes: Vec<u8> = key_info
            .public_key
            .ok_or_else(|| anyhow::anyhow!("kms public key missing"))?
            .into();

        // convert to uncompressed form
        let verifying_key = VerifyingKey::from_public_key_der(&der_bytes)?;
        let encoded_point = verifying_key.to_encoded_point(false);

        Ok(encoded_point.as_bytes().to_vec())
    }
}

#[derive(Error, Debug)]
#[error(transparent)]
pub struct Error(#[from] anyhow::Error);

#[async_trait]
impl eigenda::Sign for crate::eigen::kms::Signer {
    type Error = Error;

    async fn sign_digest(&self, message: &Message) -> Result<RecoverableSignature, Self::Error> {
        let digest_bytes: &[u8; 32] = message;

        let sign_response = self
            .client
            .sign()
            .key_id(&self.key_id)
            .message(Blob::new(digest_bytes))
            .message_type(aws_sdk_kms::types::MessageType::Digest)
            .signing_algorithm(aws_sdk_kms::types::SigningAlgorithmSpec::EcdsaSha256)
            .send()
            .await
            .context("while requesting KMS to sign the digest")?;

        let signature_der = sign_response
            .signature
            .ok_or_else(|| anyhow::anyhow!("Signature missing from KMS response"))?
            .into_inner();

        let k256_sig = k256::ecdsa::Signature::from_der(&signature_der)
            .context("Failed to parse DER signature")?;

        let k256_sig_normalized = k256_sig.normalize_s().unwrap_or(k256_sig);

        let mut sig: [u8; 65] = [0; 65];
        sig.copy_from_slice(k256_sig_normalized.to_bytes().as_ref());

        let standard_recoverable_sig = rust_eigenda_signers::RecoverableSignature::from_bytes(&sig)
            .context("Failed to create recoverable signature")?;

        Ok(standard_recoverable_sig.into())
    }

    fn public_key(&self) -> rust_eigenda_signers::PublicKey {
        self.public_key.into()
    }
}
