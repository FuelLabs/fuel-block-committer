use anyhow::Context;
use anyhow::Result;
use e2e_helpers::kms::Kms;
use e2e_helpers::kms::KmsProcess;
use ethereum_types::H160;
use k256::ecdsa::SigningKey as K256SigningKey;
use rand::RngCore;
use rand::rngs::OsRng;
use rust_eigenda_client::Sign;
use rust_eigenda_signers::secp256k1::Message;
use rust_eigenda_signers::secp256k1::PublicKey;
use rust_eigenda_signers::secp256k1::SecretKey;
use rust_eigenda_signers::secp256k1::ecdsa::RecoverableSignature;
use rust_eigenda_signers::signers::private_key::Signer as PrivateKeySigner;
use secp256k1::Secp256k1;
use sha2::{Digest, Sha256};

use rust_eigenda_client::{
    client::{BlobProvider, EigenClient},
    config::{EigenConfig, SecretUrl, SrsPointsSource},
};
use signers::eigen::kms::Signer;
use std::{env, error::Error, str::FromStr, sync::Arc, time::Duration};
use tracing::{error, info, instrument};
use tracing_subscriber::EnvFilter;
use url::Url;

async fn setup_kms_and_signer() -> Result<(KmsProcess, PrivateKeySigner, Signer)> {
    let kms_proc = Kms::default().with_show_logs(false).start().await?;

    let k256_secret_key = k256::SecretKey::random(&mut OsRng);
    let k256_signing_key = K256SigningKey::from(&k256_secret_key);

    let secp_secret_key = SecretKey::from_slice(&k256_secret_key.to_bytes())
        .expect("Failed to create secp256k1 secret key from k256 bytes");
    let local_signer = PrivateKeySigner::new(secp_secret_key);

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
    let recovered_pk = secp
        .recover_ecdsa(message, rec_sig)
        .context("Failed to recover public key")?;

    if &recovered_pk == expected_pubkey {
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
    let message =
        Message::from_slice(&message_hash_array).expect("Failed to create Message from digest");

    let rec_sig = aws_signer
        .sign_digest(&message)
        .await
        .context("Signing with AwsKmsSigner failed")?;

    let expected_pubkey = local_signer.public_key();

    verify_signature_recovery(&rec_sig.0, &message, &expected_pubkey)
        .context("Signature verification failed")?;

    Ok(())
}

// Dummy Blob Provider implementation (same as example)
#[derive(Debug)]
struct DummyBlobProvider;

#[async_trait::async_trait]
impl BlobProvider for DummyBlobProvider {
    async fn get_blob(
        &self,
        _blob_id: &str,
    ) -> Result<Option<Vec<u8>>, Box<dyn Error + Send + Sync>> {
        // This provider doesn't store or retrieve blobs in this example
        Ok(None)
    }
}

async fn initialize_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();
}

#[tokio::test]
async fn test_dispatch_3mb_blob_aws_signer() -> Result<()> {
    initialize_tracing().await;
    info!("Starting EigenDA 3MB blob dispatch test with AWS Signer...");

    // 1. Read AWS Secret Name from environment variable
    let secret_name = env::var("EIGEN_KEY")
        .context("Failed to read EIGEN_KEY (expected AWS Secret Name) environment variable")?;
    info!(secret_name = %secret_name, "Using AWS Secret Name from EIGEN_KEY");

    // 2. Initialize AWS Signer
    let kms_proc = Kms::default().with_show_logs(false).start().await?;

    // Convert hex string to bytes
    let key_bytes: [u8; 32] = hex::decode(&secret_name).unwrap().try_into().unwrap();

    // Create SecretKey
    let secret_key = k256::elliptic_curve::SecretKey::from_slice(&key_bytes)?;
    let k256_signing_key = k256::ecdsa::SigningKey::from(&secret_key);

    let kms_key_id = kms_proc.inject_secp256k1_key(&k256_signing_key).await?;

    let aws_signer = kms_proc.eigen_signer(kms_key_id).await?;

    info!("AwsSigner initialized successfully.");

    // 3. Define RPC URLs and Configuration (using Holesky testnet)
    let disperser_rpc_url = "https://disperser-holesky.eigenda.xyz".to_string();
    let eth_rpc_str = "https://ethereum-holesky-rpc.publicnode.com";
    let eth_rpc_url = SecretUrl::new(
        Url::from_str(eth_rpc_str)
            .with_context(|| format!("Invalid Ethereum RPC URL: {}", eth_rpc_str))?,
    );

    // Holesky Service Manager Address
    let svc_manager_address = H160::from_str("d4a7e1bd8015057293f0d0a557088c286942e84b")
        .context("Failed to parse service manager address")?;

    // Using remote SRS points for simplicity in e2e test
    let srs_g1_url = "https://github.com/Layr-Labs/eigenda-proxy/raw/2fd70b99ef5bf137d7bbca3461cf9e1f2c899451/resources/g1.point".to_string();
    let srs_g2_url = "https://github.com/Layr-Labs/eigenda-proxy/raw/2fd70b99ef5bf137d7bbca3461cf9e1f2c899451/resources/g2.point.powerOf2".to_string();

    let config = EigenConfig::new(
        disperser_rpc_url,
        eth_rpc_url,
        8, // Confirmation depth
        svc_manager_address,
        false, // Don't wait for finalization
        true,  // Use authenticated disperser endpoints
        SrsPointsSource::Url((srs_g1_url, srs_g2_url)),
        vec![], // Default quorums
    )
    .context("Failed to create EigenConfig")?;

    info!("Configuration prepared. Initializing EigenClient...");

    // 4. Create EigenClient instance
    let blob_provider = Arc::new(DummyBlobProvider);
    let client = EigenClient::new(config, aws_signer, blob_provider)
        .await
        .context("Failed to initialize EigenClient")?;

    info!("EigenClient initialized successfully.");

    // 5. Define 3MB blob data
    // Max original size = floor(3,145,728 * 31 / 32) = 3,047,424 bytes.
    const MAX_BLOB_SIZE: usize = 3_047_424;
    let mut blob_data = vec![0u8; MAX_BLOB_SIZE];
    OsRng.fill_bytes(&mut blob_data); // Use OsRng for cryptographic randomness

    info!(
        size = blob_data.len(),
        "Generated random blob data. Dispatching..."
    );

    // 6. Dispatch the blob
    let blob_id = match client.dispatch_blob(blob_data).await {
        Ok(blob_id) => {
            info!("---------------------------------------------------");
            info!("Blob successfully dispatched!");
            info!(blob_id = %blob_id, "Blob ID received.");
            info!("---------------------------------------------------");
            blob_id
        }
        Err(e) => {
            error!("---------------------------------------------------");
            error!("Failed to dispatch blob: {:?}", e);
            error!("---------------------------------------------------");
            return Err(anyhow::anyhow!("Blob dispatch failed: {}", e));
        }
    };

    // 7. Poll for inclusion data
    let polling_timeout = Duration::from_secs(300); // 5 minutes
    let polling_interval = Duration::from_secs(10); // 10 seconds
    let start_time = tokio::time::Instant::now();

    info!(
        timeout = ?polling_timeout,
        interval = ?polling_interval,
        "Polling for inclusion data..."
    );

    loop {
        if start_time.elapsed() > polling_timeout {
            error!("Polling timed out after {:?}", polling_timeout);
            return Err(anyhow::anyhow!(
                "Polling for inclusion data timed out for blob ID: {}",
                blob_id
            ));
        }

        match client.get_inclusion_data(&blob_id).await {
            Ok(Some(data)) => {
                info!("---------------------------------------------------");
                info!("Inclusion data retrieved successfully!");
                info!(blob_id = %blob_id);
                info!(inclusion_data = ?data, "Inclusion data details.");
                info!("---------------------------------------------------");
                // Test passes if inclusion data is found
                assert!(true, "Inclusion data successfully retrieved.");
                break;
            }
            Ok(None) => {
                info!(blob_id = %blob_id, "Blob inclusion data not found yet. Retrying in {:?}...", polling_interval);
                tokio::time::sleep(polling_interval).await;
            }
            Err(e) => {
                error!(blob_id = %blob_id, error = ?e, "Error polling for inclusion data");
                // Depending on the error, we might want to retry or fail immediately
                // For now, let's fail on persistent errors during polling
                return Err(anyhow::anyhow!(
                    "Error polling for inclusion data for blob ID {}: {}",
                    blob_id,
                    e
                ));
            }
        }
    }

    Ok(())
}
