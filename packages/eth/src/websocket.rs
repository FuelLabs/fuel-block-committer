use std::num::NonZeroU32;

use ::metrics::{prometheus::core::Collector, HealthChecker, RegistersMetrics};
use alloy::primitives::Address;
use ports::{
    l1::{FragmentsSubmitted, Result},
    types::{Fragment, NonEmpty, TransactionResponse, U256},
};
use url::Url;

pub use self::event_streamer::EthEventStreamer;
use self::{
    connection::WsConnection,
    health_tracking_middleware::{EthApi, HealthTrackingMiddleware},
};
use crate::{AwsClient, FirstTxFeeOverride};

mod connection;
mod event_streamer;
mod health_tracking_middleware;

#[derive(Clone)]
pub struct WebsocketClient {
    inner: HealthTrackingMiddleware<WsConnection>,
}

impl WebsocketClient {
    pub async fn connect(
        url: Url,
        contract_address: Address,
        main_key_arn: String,
        blob_pool_key_arn: Option<String>,
        unhealthy_after_n_errors: usize,
        aws_client: AwsClient,
        first_tx_fee_override: Option<FirstTxFeeOverride>,
    ) -> ports::l1::Result<Self> {
        let blob_signer = if let Some(key_arn) = blob_pool_key_arn {
            Some(aws_client.make_signer(key_arn).await?)
        } else {
            None
        };

        let main_signer = aws_client.make_signer(main_key_arn).await?;

        let provider = WsConnection::connect(
            url,
            contract_address,
            main_signer,
            blob_signer,
            first_tx_fee_override,
        )
        .await?;

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

    pub(crate) async fn submit(&self, hash: [u8; 32], height: u32) -> Result<()> {
        Ok(self.inner.submit(hash, height).await?)
    }

    pub(crate) fn commit_interval(&self) -> NonZeroU32 {
        self.inner.commit_interval()
    }

    pub(crate) async fn _get_block_number(&self) -> Result<u64> {
        Ok(self.inner.get_block_number().await?)
    }

    pub(crate) async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>> {
        Ok(self.inner.get_transaction_response(tx_hash).await?)
    }

    pub(crate) async fn balance(&self) -> Result<U256> {
        Ok(self.inner.balance().await?)
    }

    pub(crate) async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
    ) -> ports::l1::Result<FragmentsSubmitted> {
        Ok(self.inner.submit_state_fragments(fragments).await?)
    }

    #[cfg(feature = "test-helpers")]
    pub async fn finalized(&self, hash: [u8; 32], height: u32) -> Result<bool> {
        Ok(self.inner.finalized(hash, height).await?)
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
