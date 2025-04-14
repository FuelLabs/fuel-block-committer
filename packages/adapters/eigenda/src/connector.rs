use std::{
    num::NonZeroU32,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use async_trait::async_trait;
use byte_unit::Byte;
use ethereum_types::H160;
use futures::{StreamExt, TryFutureExt};
use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
};
use rust_eigenda_client::{
    client::{BlobProvider, EigenClient},
    config::{EigenConfig, EigenSecrets, PrivateKey, SecretUrl, SrsPointsSource},
};
use secp256k1::SecretKey;
use services::{
    Error as ServiceError, Result as ServiceResult,
    types::{DispersalStatus, EigenDASubmission, Fragment},
};
use signers::KeySource;
use tokio::sync::RwLock;
use tracing::{info, warn};
use url::Url;

use crate::{
    codec::convert_by_padding_empty_byte,
    error::{Error, Result},
};

impl services::state_committer::port::eigen_da::Api for EigenDAClient {
    async fn submit_state_fragment(&self, fragment: Fragment) -> ServiceResult<EigenDASubmission> {
        let data = fragment.data;
        let start = Instant::now();
        self.throughput_limiter
            .until_n_ready(NonZeroU32::new(data.len() as u32).unwrap())
            .await
            .unwrap();
        self.post_frequency_limiter
            .until_n_ready(NonZeroU32::new(1).unwrap())
            .await
            .unwrap();

        let elapsed = start.elapsed();
        if elapsed > Duration::from_millis(100) {
            let elapsed = humantime::format_duration(elapsed);
            info!("Was throttled for {elapsed}");
        }

        let data: Vec<_> = data.into_iter().collect();

        let start = Instant::now();
        let data_len = data.len();

        // Use the new client to dispatch the blob (padding is applied inside dispatch_blob)
        let blob_id = self
            .dispatch_blob(data)
            .await
            .map_err(|e| ServiceError::Other(format!("Failed to disperse state fragment: {e}")))?;

        let original_size =
            Byte::from_u64(data_len as u64).get_appropriate_unit(byte_unit::UnitType::Decimal);

        let bytes_per_sec = data_len as f64 / start.elapsed().as_secs_f64();
        let speed =
            Byte::from_u64(bytes_per_sec as u64).get_appropriate_unit(byte_unit::UnitType::Decimal);
        let elapsed = humantime::format_duration(start.elapsed());

        info!("Posted {original_size:.3} in {elapsed} at speed: {speed:.5}");

        Ok(EigenDASubmission {
            request_id: blob_id.as_bytes().to_vec(),
            created_at: None,
            status: DispersalStatus::Processing,
            ..Default::default()
        })
    }
}

impl services::state_listener::port::eigen_da::Api for EigenDAClient {
    fn get_blob_status(
        &self,
        id: Vec<u8>,
    ) -> impl ::core::future::Future<Output = ServiceResult<DispersalStatus>> + Send {
        async move {
            let blob_id = String::from_utf8(id.clone())
                .map_err(|e| ServiceError::Other(format!("Invalid blob ID format: {e}")))?;

            let status = self.check_blob_status(&blob_id).await?;
            Ok(status)
        }
    }
}

// Define a DummyBlobProvider as we're not retrieving blobs in the committer
#[derive(Debug, Clone)]
struct DummyBlobProvider;

#[async_trait]
impl BlobProvider for DummyBlobProvider {
    async fn get_blob(
        &self,
        _blob_id: &str,
    ) -> std::result::Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        // This provider doesn't store or retrieve blobs in this example
        Ok(None)
    }
}

#[derive(Debug, Clone)]
pub struct EigenDAClient {
    eigen_client: Arc<RwLock<EigenClient>>,
    // Limits the number of bytes that can be posted per second.
    throughput_limiter: Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>>,
    // Limits the posting frequency to one request per second.
    post_frequency_limiter: Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>>,
}

#[derive(Debug, Clone, Copy)]
pub struct Throughput {
    pub bytes_per_sec: NonZeroU32,
    pub max_burst: NonZeroU32,
    pub calls_per_sec: NonZeroU32,
}

impl EigenDAClient {
    pub async fn new(key: KeySource, rpc: Url, throughput: Throughput) -> Result<Self> {
        // Extract key value from KeySource
        let key_str = match key {
            KeySource::Private(hex) => hex,
            KeySource::Kms(_) => {
                return Err(Error::Other(
                    "AWS KMS keys are not supported for EigenDA".to_string(),
                ));
            }
        };

        // Ensure the key is valid
        if SecretKey::from_str(&key_str).is_err() {
            return Err(Error::Other("Invalid private key format".to_string()));
        }

        let private_key = PrivateKey::from_str(&key_str)
            .map_err(|e| Error::Other(format!("Failed to parse private key: {e}")))?;

        // Set up Ethereum RPC URL
        // For now, we're using the same URL for disperser and eth - this may need to be revised
        let disperser_rpc_url = rpc.to_string();

        // Set a default Holesky RPC endpoint for Ethereum interaction
        // This could be changed to a configurable parameter
        let eth_rpc_str = "https://ethereum-holesky-rpc.publicnode.com";
        // TODO: segfault
        let eth_rpc_url = SecretUrl::new(
            Url::parse(eth_rpc_str)
                .map_err(|e| Error::Other(format!("Invalid Ethereum RPC URL: {e}")))
                .expect("TODO: segfault"),
        );

        // Placeholder Service Manager Address - this should be configured properly for production
        let svc_manager_address = H160::from_str("d4a7e1bd8015057293f0d0a557088c286942e84b")
            .map_err(|e| Error::Other(format!("Invalid service manager address: {e}")))?;

        // Default SRS points URLs from Eigen-DA repos
        let srs_g1_url = "https://github.com/Layr-Labs/eigenda-proxy/raw/2fd70b99ef5bf137d7bbca3461cf9e1f2c899451/resources/g1.point".to_string();
        let srs_g2_url = "https://github.com/Layr-Labs/eigenda-proxy/raw/2fd70b99ef5bf137d7bbca3461cf9e1f2c899451/resources/g2.point.powerOf2".to_string();

        let config = EigenConfig::new(
            disperser_rpc_url,
            eth_rpc_url,
            8, // Set confirmation depth as needed
            svc_manager_address,
            false, // Set true to wait for Ethereum finalization
            true,  // Set true if using authenticated disperser endpoints
            SrsPointsSource::Url((srs_g1_url, srs_g2_url)),
            vec![], // Specify quorum IDs if using custom quorums
        )
        .map_err(|e| Error::Other(format!("Failed to create EigenConfig: {e}")))?;

        let secrets = EigenSecrets { private_key };

        // Create EigenClient instance
        let blob_provider = Arc::new(DummyBlobProvider);
        let eigen_client = EigenClient::new(config, secrets, blob_provider)
            .await
            .map_err(|e| Error::Other(format!("Failed to initialize EigenClient: {e}")))?;

        let throughput_quota =
            Quota::per_second(throughput.bytes_per_sec).allow_burst(throughput.max_burst);
        let post_quota = Quota::per_second(throughput.calls_per_sec);

        Ok(Self {
            eigen_client: Arc::new(RwLock::new(eigen_client)),
            throughput_limiter: Arc::new(RateLimiter::direct(throughput_quota)),
            post_frequency_limiter: Arc::new(RateLimiter::direct(post_quota)),
        })
    }

    async fn dispatch_blob(&self, data: Vec<u8>) -> Result<String> {
        let client = self.eigen_client.read().await;

        // Use the padding function to ensure proper data format for EigenDA
        let padded_data = convert_by_padding_empty_byte(&data);

        // Use the client to dispatch the blob
        let blob_id = client
            .dispatch_blob(padded_data)
            .await
            .map_err(|e| Error::Other(format!("Failed to dispatch blob: {e}")))?;

        Ok(blob_id)
    }

    async fn check_blob_status(&self, blob_id: &str) -> Result<DispersalStatus> {
        let client = self.eigen_client.read().await;

        // Check if the blob has inclusion data (is confirmed/finalized)
        match client.get_inclusion_data(blob_id).await {
            Ok(Some(_)) => {
                // If we have inclusion data, the blob is finalized
                Ok(DispersalStatus::Finalized)
            }
            Ok(None) => {
                // If we don't have inclusion data yet, the blob is being processed
                Ok(DispersalStatus::Processing)
            }
            Err(e) => {
                // If there's an error, consider the blob processing state as failed
                warn!("Error checking blob status for {}: {}", blob_id, e);
                Ok(DispersalStatus::Failed)
            }
        }
    }
}
