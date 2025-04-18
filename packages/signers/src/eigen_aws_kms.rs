use anyhow::Context;
use aws_sdk_kms::{primitives::Blob, Client};
use k256::{ecdsa::VerifyingKey, pkcs8::DecodePublicKey};
use rust_eigenda_signers::{secp256k1::Message, RecoverableSignature, SignerError};

#[derive(Clone)]
pub struct AwsKmsSigner {
    key_id: String,
    client: Client,
    public_key: rust_eigenda_signers::secp256k1::PublicKey,
    k256_verifying_key: VerifyingKey, // Store k256 key for internal use
}

impl AwsKmsSigner {
    pub async fn new(client: Client, key_id: String) -> anyhow::Result<Self> {
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

        Ok(AwsKmsSigner {
            key_id,
            client,
            public_key: secp_pub_key,
            k256_verifying_key: k256_pub_key, // Store k256 key for internal use
        })
    }
}

// Implement Debug manually to avoid showing the client details fully
impl std::fmt::Debug for AwsKmsSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsKmsSigner")
            .field("key_id", &self.key_id)
            .field("public_key", &self.public_key)
            .field("k256_verifying_key", &self.k256_verifying_key)
            .field("client", &"aws_sdk_kms::Client { ... }") // Avoid printing potentially large client info
            .finish()
    }
}

#[async_trait::async_trait]
impl rust_eigenda_signers::Signer for AwsKmsSigner {
    async fn sign_digest(&self, message: &Message) -> Result<RecoverableSignature, SignerError> {
        let digest_bytes: &[u8; 32] = message.as_ref();

        let sign_response = self
            .client
            .sign()
            .key_id(&self.key_id)
            .message(Blob::new(digest_bytes))
            .message_type(aws_sdk_kms::types::MessageType::Digest)
            .signing_algorithm(aws_sdk_kms::types::SigningAlgorithmSpec::EcdsaSha256)
            .send()
            .await // Ensure .await is before map_err
            .map_err(|e| SignerError::SignerSpecific(Box::new(e)))?;

        let signature_der = sign_response
            .signature
            .ok_or_else(|| {
                SignerError::SignerSpecific(
                    anyhow::anyhow!("Signature missing from KMS response").into(),
                )
            })?
            .into_inner();

        let k256_sig = k256::ecdsa::Signature::from_der(&signature_der)
            .map_err(|e| SignerError::SignerSpecific(Box::new(e)))?;

        let k256_sig_normalized = k256_sig.normalize_s().unwrap_or(k256_sig);

        let k256_recid = determine_k256_recovery_id(
            &k256_sig_normalized,
            digest_bytes,
            &self.k256_verifying_key, // Use stored k256 key for internal use
        )
        .map_err(|e| SignerError::SignerSpecific(e.into()))?;

        let secp_sig = rust_eigenda_signers::secp256k1::ecdsa::Signature::from_compact(
            &k256_sig_normalized.to_bytes(),
        )
        .map_err(|e| SignerError::SignerSpecific(Box::new(e)))?;

        let secp_recid = rust_eigenda_signers::secp256k1::ecdsa::RecoveryId::from_i32(
            k256_recid.to_byte() as i32,
        )
        .map_err(|e| SignerError::SignerSpecific(Box::new(e)))?;

        let standard_recoverable_sig =
            rust_eigenda_signers::secp256k1::ecdsa::RecoverableSignature::from_compact(
                secp_sig.serialize_compact().as_slice(),
                secp_recid,
            )
            .map_err(|e| SignerError::SignerSpecific(Box::new(e)))?;

        Ok(standard_recoverable_sig.into())
    }

    fn public_key(&self) -> rust_eigenda_signers::secp256k1::PublicKey {
        self.public_key
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
