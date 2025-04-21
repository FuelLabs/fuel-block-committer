use alloy::network::TxSigner;
use anyhow::Context;
use async_trait::async_trait;
use aws_config::{Region, SdkConfig, default_provider::credentials::DefaultCredentialsChain};
#[cfg(feature = "test-helpers")]
use aws_sdk_kms::config::Credentials;
use aws_sdk_kms::primitives::Blob;
use aws_sdk_kms::{Client as InnerClient, config::BehaviorVersion};
use k256::{
    ecdsa::{RecoveryId, Signature, VerifyingKey},
    pkcs8::DecodePublicKey,
};
use rust_eigenda_signers::{RecoverableSignature, secp256k1::Message};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct Signer {
    key_id: String,
    client: InnerClient,
    public_key: rust_eigenda_signers::secp256k1::PublicKey,
    k256_verifying_key: VerifyingKey,
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
        let secp_pub_key = rust_eigenda_signers::secp256k1::PublicKey::from_slice(
            k256_pub_key.to_encoded_point(false).as_bytes(),
        )
        .context("Failed to convert k256 pubkey to secp256k1 pubkey")?;

        Ok(Self {
            key_id,
            client,
            public_key: secp_pub_key,
            k256_verifying_key: k256_pub_key,
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
        let digest_bytes: &[u8; 32] = message.as_ref();

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

        let k256_recid = determine_k256_recovery_id(
            &k256_sig_normalized,
            digest_bytes,
            &self.k256_verifying_key, // Use stored k256 key for internal use
        )?;

        let secp_sig = rust_eigenda_signers::secp256k1::ecdsa::Signature::from_compact(
            &k256_sig_normalized.to_bytes(),
        )
        .context("Failed to convert k256 signature to secp256k1 signature")?;

        let secp_recid = rust_eigenda_signers::secp256k1::ecdsa::RecoveryId::from_i32(
            k256_recid.to_byte() as i32,
        )
        .context("Failed to convert k256 recovery ID to secp256k1 recovery ID")?;

        let standard_recoverable_sig =
            rust_eigenda_signers::secp256k1::ecdsa::RecoverableSignature::from_compact(
                secp_sig.serialize_compact().as_slice(),
                secp_recid,
            )
            .context("Failed to create recoverable signature")?;

        Ok(standard_recoverable_sig.into())
    }

    fn public_key(&self) -> rust_eigenda_signers::PublicKey {
        self.public_key.into()
    }
}

// Helper function to determine k256 recovery ID.
// Necessary because KMS returns a DER signature, and we need to extract R, S, and find V.
fn determine_k256_recovery_id(
    sig: &k256::ecdsa::Signature,
    message_hash: &[u8; 32],
    expected_pubkey: &VerifyingKey,
) -> anyhow::Result<k256::ecdsa::RecoveryId> {
    let recid_0 = k256::ecdsa::RecoveryId::from_byte(0).context("Bad RecoveryId byte 0")?;
    let recid_1 = k256::ecdsa::RecoveryId::from_byte(1).context("Bad RecoveryId byte 1")?;

    if let Ok(recovered_key) = VerifyingKey::recover_from_prehash(message_hash, sig, recid_0) {
        if &recovered_key == expected_pubkey {
            return Ok(recid_0);
        }
    }

    if let Ok(recovered_key) = VerifyingKey::recover_from_prehash(message_hash, sig, recid_1) {
        if &recovered_key == expected_pubkey {
            return Ok(recid_1);
        }
    }

    anyhow::bail!("Could not recover correct public key from k256 signature")
}
