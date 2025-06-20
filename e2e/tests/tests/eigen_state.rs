use anyhow::Result;
use e2e_helpers::whole_stack::{
    create_and_fund_kms_signers, deploy_contract, start_db, start_eigen_committer, start_eth,
    start_fuel_node, start_kms,
};
use k256::ecdsa::SigningKey as K256SigningKey;
use std::time::Duration;
use tracing::info;

#[tokio::test]
async fn test_eigen_state() -> Result<()> {
    // Start required services
    let logs = false;
    let kms = start_kms(logs).await?;
    let eth_node = start_eth(logs).await?;
    let eth_signers = create_and_fund_kms_signers(&kms, &eth_node).await?;

    // Get Eigen key from environment and inject into KMS
    let eigen_key_hex = std::env::var("EIGEN_KEY")
        .expect("EIGEN_KEY environment variable must be set for Eigen tests");

    // Convert hex string to bytes
    let key_bytes: [u8; 32] = hex::decode(&eigen_key_hex)
        .expect("Failed to decode EIGEN_KEY hex")
        .try_into()
        .expect("EIGEN_KEY must be 32 bytes");

    // Create signing key and inject into KMS
    let secret_key = k256::elliptic_curve::SecretKey::from_slice(&key_bytes)?;
    let k256_signing_key = K256SigningKey::from(&secret_key);
    let kms_key_id = kms.inject_secp256k1_key(&k256_signing_key).await?;

    // Deploy contract and start services
    let request_timeout = Duration::from_secs(50);
    let max_fee = 1_000_000_000_000;
    let (_contract_args, deployed_contract) =
        deploy_contract(&eth_node, eth_signers.clone(), max_fee, request_timeout).await?;
    let db = start_db().await?;
    let fuel_node = start_fuel_node(logs, Some(Duration::from_millis(200))).await?;

    // Start Eigen committer with KMS key
    let logs = true;
    let committer = start_eigen_committer(
        logs,
        db.clone(),
        &eth_node,
        fuel_node.url(),
        &deployed_contract,
        eth_signers.main,
        kms_key_id, // Use the KMS key ID instead of raw EIGEN_KEY
        "1 KB",
    )
    .await?;

    info!("waiting for 10s.");
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Test 1: Verify committer is running by checking metrics endpoint
    let metrics_url = committer.metrics_url();
    let client = reqwest::Client::new();
    let response = client.get(metrics_url.clone()).send().await?;
    assert!(
        response.status().is_success(),
        "Metrics endpoint should be accessible"
    );

    // Test 2: Verify state synchronization
    // Wait for some blocks to be processed
    tokio::time::sleep(Duration::from_secs(100)).await;

    // TODO: we should investigate directly querying the database instead of using metrics.
    // Check if committer has processed any blocks
    let metrics = client.get(metrics_url).send().await?.text().await?;
    assert!(
        metrics.contains("tsize_eigen_submissions 1"),
        "Committer should have submitted some fragments to Eigen"
    );

    println!("metrics: {}", metrics);

    Ok(())
}
