use std::{
    num::NonZeroU32,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use byte_unit::Byte;
use ethereum_types::H160;
use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
};
pub use rust_eigenda_v2_client::rust_eigenda_signers::Sign;
use rust_eigenda_v2_client::{
    core::BlobKey,
    disperser_client::{DisperserClient, DisperserClientConfig},
    payload_disperser::{PayloadDisperser, PayloadDisperserConfig},
    utils::SecretUrl,
};
use rust_eigenda_v2_common::{Payload, PayloadForm};
use services::{
    Error as ServiceError, Result as ServiceResult,
    types::{DispersalStatus, EigenDASubmission, Fragment},
};
use tracing::{info, warn};
use url::Url;

use crate::{
    bindings::BlobStatus,
    codec::convert_by_padding_empty_byte,
    error::{Error, Result},
};

#[derive(Debug, Clone)]
struct EigenClient<S> {
    /// The payload disperser instance that handles the actual payload dispatching.
    payload_disperser: PayloadDisperser<S>,
    /// The disperser client that is able to get the status of the blobs.
    disperser_client: DisperserClient<S>,
}

impl<S> EigenClient<S>
where
    S: Sign + Clone,
{
    async fn new(payload_disperser_cfg: PayloadDisperserConfig, signer: S) -> anyhow::Result<Self> {
        let disperser_client = DisperserClient::new(DisperserClientConfig {
            disperser_rpc: payload_disperser_cfg.disperser_rpc.clone(),
            signer: signer.clone(),
            use_secure_grpc_flag: payload_disperser_cfg.use_secure_grpc_flag,
        })
        .await?;
        let payload_disperser = PayloadDisperser::new(payload_disperser_cfg, signer).await?;

        Ok(Self {
            payload_disperser,
            disperser_client,
        })
    }
}

impl<S> services::state_committer::port::eigen_da::Api for EigenDAClient<S>
where
    S: Sign + Clone,
{
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
            id: None,
            request_id: blob_id.as_bytes().to_vec(),
            created_at: None,
            status: DispersalStatus::Processing,
        })
    }
}

impl<S> services::state_listener::port::eigen_da::Api for EigenDAClient<S>
where
    S: Clone + Sign + Send + Sync,
{
    async fn get_blob_status(&self, id: Vec<u8>) -> ServiceResult<DispersalStatus> {
        let blob_id = String::from_utf8(id.clone())
            .map_err(|e| ServiceError::Other(format!("Invalid blob ID format: {e}")))?;

        let status = self.check_blob_status(&blob_id).await?;
        Ok(status)
    }
}

#[derive(Debug, Clone)]
pub struct EigenDAClient<S> {
    eigen_client: EigenClient<S>,
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

impl<S> EigenDAClient<S>
where
    S: Sign + Clone,
{
    pub async fn new(signer: S, rpc: Url, throughput: Throughput) -> Result<Self> {
        // Set up Ethereum RPC URL
        // For now, we're using the same URL for disperser and eth - this may need to be revised
        let disperser_rpc_url = rpc.to_string();

        // Set a default Holesky RPC endpoint for Ethereum interaction
        // This could be changed to a configurable parameter
        let eth_rpc_str = "https://ethereum-holesky-rpc.publicnode.com";
        let eth_rpc_url =
            SecretUrl::new(Url::parse(eth_rpc_str).map_err(Error::InvalidRPCUrl)?);

        // TODO: make configurable
        const CERT_VERIFIER_ADDRESS: &str = "fe52fe1940858dcb6e12153e2104ad0fdfbe1162"; // holesky cert verifier address

        let config = PayloadDisperserConfig {
            polynomial_form: PayloadForm::Coeff,
            blob_version: 0,
            cert_verifier_address: H160::from_str(CERT_VERIFIER_ADDRESS).expect("qed"),
            eth_rpc_url: eth_rpc_url.clone(),
            disperser_rpc: disperser_rpc_url,
            use_secure_grpc_flag: true,
        };

        // Create PayloadDisperser instance
        let eigen_client = EigenClient::new(config, signer)
            .await
            .map_err(Error::EigenDAClientInit)?;

        let throughput_quota =
            Quota::per_second(throughput.bytes_per_sec).allow_burst(throughput.max_burst);
        let post_quota = Quota::per_second(throughput.calls_per_sec);

        Ok(Self {
            eigen_client,
            throughput_limiter: Arc::new(RateLimiter::direct(throughput_quota)),
            post_frequency_limiter: Arc::new(RateLimiter::direct(post_quota)),
        })
    }

    pub async fn dispatch_blob(&self, data: Vec<u8>) -> Result<String>
    where
        S: Sign,
    {
        // Use the padding function to ensure proper data format for EigenDA
        let padded_data = convert_by_padding_empty_byte(&data);

        // Use the client to dispatch the blob
        let blob_id = self
            .eigen_client
            .payload_disperser
            .send_payload(Payload::new(padded_data))
            .await
            .map_err(Error::BlockDispatchFailed)?;

        Ok(blob_id.to_hex())
    }

    pub async fn check_blob_status(&self, blob_id: &str) -> Result<DispersalStatus> {
        let blob_key = BlobKey::from_hex(blob_id).map_err(Error::InvalidBlobKey)?;

        let response = match self
            .eigen_client
            .disperser_client
            .blob_status(&blob_key)
            .await
        {
            Ok(response) => response,
            Err(e) => {
                warn!("Error checking blob status for {}: {}", blob_id, e);
                return Ok(DispersalStatus::Failed);
            }
        };

        let dispersal_status = match BlobStatus::from(response.status) {
            BlobStatus::Failed => {
                warn!("Blob {} failed to disperse", blob_id);
                DispersalStatus::Failed
            }
            other_status => other_status.into(),
        };

        Ok(dispersal_status)
    }
}
