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

#[derive(Clone)]
pub struct AwsClient {
    client: KmsClient,
}

impl AwsClient {
    pub fn try_new(
        region: String,
        access_key_id: String,
        secret_access_key: String,
        allow_http: bool,
    ) -> ports::l1::Result<Self> {
        let credentials = StaticProvider::new_minimal(access_key_id, secret_access_key);
        let region: Region = serde_json::from_str(&region).map_err(|err| {
            ports::l1::Error::Other(format!(
                "Could not parse {region} into a aws Region. Reason: {err}"
            ))
        })?;

        let dispatcher = if allow_http {
            let hyper_builder = hyper::client::Client::builder();
            let http_connector = hyper_rustls::HttpsConnectorBuilder::new()
                .with_native_roots()
                .https_or_http()
                .enable_http1()
                .build();
            HttpClient::from_builder(hyper_builder, http_connector)
        } else {
            HttpClient::new().map_err(|e| {
                ports::l1::Error::Network(format!("Could not create http client: {e}"))
            })?
        };

        let client = KmsClient::new_with(dispatcher, credentials, region);
        Ok(Self { client })
    }

    pub fn inner(&self) -> &KmsClient {
        &self.client
    }

    pub async fn make_signer(&self, key_id: String, chain_id: u64) -> ports::l1::Result<AwsSigner> {
        AwsSigner::new(self.client.clone(), key_id, chain_id)
            .await
            .map_err(|err| ports::l1::Error::Other(format!("Error making aws signer: {err}")))
    }
}

impl WebsocketClient {
    pub async fn connect(
        url: &Url,
        chain_id: Chain,
        contract_address: Address,
        main_key_id: String,
        blob_pool_key_id: Option<String>,
        unhealthy_after_n_errors: usize,
        aws_client: AwsClient,
    ) -> ports::l1::Result<Self> {
        let main_signer = aws_client.make_signer(main_key_id, chain_id.into()).await?;

        let blob_signer = if let Some(key_id) = blob_pool_key_id {
            Some(
                aws_client
                    .make_signer(key_id.clone(), chain_id.into())
                    .await?,
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
