use std::num::NonZeroU32;

use ::metrics::{prometheus::core::Collector, HealthChecker, RegistersMetrics};
use ethers::{
    signers::AwsSigner,
    types::{Address, Chain},
};
use ports::{
    l1::Result,
    types::{ValidatedFuelBlock, U256},
};
use rusoto_core::{credential::StaticProvider, HttpClient, Region};
use rusoto_kms::KmsClient;
use url::Url;

pub use self::event_streamer::EthEventStreamer;
use self::{
    connection::WsConnection,
    health_tracking_middleware::{EthApi, HealthTrackingMiddleware},
};

mod connection;
mod event_streamer;
mod health_tracking_middleware;

#[derive(Clone)]
pub struct WebsocketClient {
    inner: HealthTrackingMiddleware<WsConnection>,
}

impl WebsocketClient {
    pub async fn connect(
        url: &Url,
        chain_id: Chain,
        contract_address: Address,
        main_key_id: String,
        blob_pool_key_id: Option<String>,
        unhealthy_after_n_errors: usize,
        aws_region: String,
        aws_access_key_id: String,
        aws_secret_access_key: String,
        aws_allow_http: bool,
    ) -> ports::l1::Result<Self> {
        let credentials = StaticProvider::new_minimal(aws_access_key_id, aws_secret_access_key);
        // TODO: segfault error here instead of unwrap
        // TODO: segfault is json desirable here ?
        let region: Region = serde_json::from_str(&aws_region).unwrap();

        let dispatcher = if aws_allow_http {
            let hyper_builder = hyper::client::Client::builder();
            let http_connector = hyper_rustls::HttpsConnectorBuilder::new()
                .with_native_roots()
                .https_or_http()
                .enable_http1()
                .build();
            HttpClient::from_builder(hyper_builder, http_connector)
        } else {
            HttpClient::new().expect("failed to create request dispatcher")
        };

        let client = KmsClient::new_with(dispatcher, credentials, region);

        let main_signer = AwsSigner::new(client.clone(), main_key_id.clone(), chain_id.into())
            .await
            .unwrap();

        let blob_signer = if let Some(key_id) = blob_pool_key_id {
            Some(
                AwsSigner::new(client.clone(), key_id.clone(), chain_id.into())
                    .await
                    .unwrap(),
            )
        } else {
            None
        };

        let provider =
            WsConnection::connect(url, contract_address, main_signer, blob_signer).await?;

        Ok(Self {
            inner: HealthTrackingMiddleware::new(provider, unhealthy_after_n_errors),
        })
    }

    #[must_use]
    pub fn connection_health_checker(&self) -> HealthChecker {
        self.inner.connection_health_checker()
    }

    pub(crate) fn event_streamer(&self, eth_block_height: u64) -> EthEventStreamer {
        self.inner.event_streamer(eth_block_height)
    }

    pub(crate) async fn submit(&self, block: ValidatedFuelBlock) -> Result<()> {
        Ok(self.inner.submit(block).await?)
    }

    pub(crate) fn commit_interval(&self) -> NonZeroU32 {
        self.inner.commit_interval()
    }

    pub(crate) async fn get_block_number(&self) -> Result<u64> {
        Ok(self.inner.get_block_number().await?)
    }

    pub(crate) async fn balance(&self) -> Result<U256> {
        Ok(self.inner.balance().await?)
    }

    pub async fn submit_l2_state(&self, tx: Vec<u8>) -> Result<[u8; 32]> {
        Ok(self.inner.submit_l2_state(tx).await?)
    }

    #[cfg(feature = "test-helpers")]
    pub async fn finalized(&self, block: ValidatedFuelBlock) -> Result<bool> {
        Ok(self.inner.finalized(block).await?)
    }

    #[cfg(feature = "test-helpers")]
    pub async fn block_hash_at_commit_height(&self, commit_height: u32) -> Result<[u8; 32]> {
        Ok(self
            .inner
            .block_hash_at_commit_height(commit_height)
            .await?)
    }
}

// User responsible for registering any metrics T might have
impl RegistersMetrics for WebsocketClient {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        self.inner.metrics()
    }
}
