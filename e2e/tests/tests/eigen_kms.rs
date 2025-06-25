use anyhow::Context;
use anyhow::Result;
use e2e_helpers::kms::Kms;
use e2e_helpers::kms::KmsProcess;
use k256::ecdsa::SigningKey as K256SigningKey;
use rand::rngs::OsRng;
use rust_eigenda_signers::Message;
use rust_eigenda_signers::PublicKey;
use rust_eigenda_signers::RecoverableSignature;
use rust_eigenda_signers::signers::private_key::Signer as PrivateKeySigner;
use rust_eigenda_v2_client::rust_eigenda_signers::Sign;
use secp256k1::Secp256k1;
use sha2::{Digest, Sha256};
use signers::eigen::kms::Signer;

async fn setup_kms_and_signer() -> Result<(KmsProcess, PrivateKeySigner, Signer)> {
    let kms_proc = Kms::default().with_show_logs(false).start().await?;

    let k256_secret_key = k256::SecretKey::random(&mut OsRng);
    let k256_signing_key = K256SigningKey::from(&k256_secret_key);

    let secp_secret_key = secp256k1::SecretKey::from_slice(k256_secret_key.to_bytes().as_slice())
        .expect("Failed to create secp256k1 secret key from k256 bytes");
    let local_signer = PrivateKeySigner::new(secp_secret_key.into());

    let kms_key_id = kms_proc.inject_secp256k1_key(&k256_signing_key).await?;

    let aws_signer = Signer::new(kms_proc.client().clone(), kms_key_id).await?;

    Ok((kms_proc, local_signer, aws_signer))
}

fn verify_signature_recovery(
    rec_sig: &RecoverableSignature,
    message: &Message,
    expected_pubkey: &PublicKey,
) -> Result<()> {
    let secp = Secp256k1::new();
    let recovery_id = secp256k1::ecdsa::RecoveryId::from_i32(rec_sig.v.to_byte() as i32)
        .context("Invalid recovery ID")?;
    let message = secp256k1::Message::from_slice(message.as_bytes())?;
    let sig: [u8; 64] = rec_sig.to_bytes().as_slice()[..64]
        .try_into()
        .context("Failed to convert signature to 64-byte array")?;
    let recoverable_sig =
        secp256k1::ecdsa::RecoverableSignature::from_compact(sig.as_slice(), recovery_id)
            .context("Failed to create recoverable signature")?;
    let recovered_pk = secp
        .recover_ecdsa(&message, &recoverable_sig)
        .context("Failed to recover public key")?;

    if &PublicKey::from(recovered_pk) == expected_pubkey {
        Ok(())
    } else {
        anyhow::bail!("Recovered public key does not match expected public key")
    }
}

#[tokio::test]
async fn test_kms_signer_public_key_and_address() -> Result<()> {
    let (_kms_proc, local_signer, aws_signer) = setup_kms_and_signer().await?;

    let expected_secp_pubkey = local_signer.public_key();
    let actual_secp_pubkey = aws_signer.public_key();
    assert_eq!(
        actual_secp_pubkey, expected_secp_pubkey,
        "Public key from AwsKmsSigner does not match the expected key from LocalSigner"
    );

    let expected_address = local_signer.public_key().address();
    let actual_address = aws_signer.public_key().address();
    assert_eq!(
        actual_address, expected_address,
        "Address from AwsKmsSigner does not match the expected address from LocalSigner"
    );

    Ok(())
}

#[tokio::test]
async fn test_kms_signer_sign_and_verify() -> Result<()> {
    let (_kms_proc, local_signer, aws_signer) = setup_kms_and_signer().await?;
    let test_message_bytes = b"Test message for KMS signer trait implementation";
    let message_hash_array: [u8; 32] = Sha256::digest(test_message_bytes).into();
    let message = Message::from(message_hash_array);

    let rec_sig = aws_signer
        .sign_digest(&message)
        .await
        .context("Signing with AwsKmsSigner failed")?;

    let expected_pubkey = local_signer.public_key();

    verify_signature_recovery(&rec_sig, &message, &expected_pubkey)
        .context("Signature verification failed")?;

    Ok(())
}
