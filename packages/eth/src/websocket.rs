use std::num::NonZeroU32;

use ::metrics::{prometheus::core::Collector, HealthChecker, RegistersMetrics};
use alloy::primitives::Address;
use ports::{
    l1::Result,
    types::{BlockSubmissionTx, TransactionResponse, ValidatedFuelBlock, U256},
};
use url::Url;

use self::{
    connection::WsConnection,
    health_tracking_middleware::{EthApi, HealthTrackingMiddleware},
};
use crate::AwsClient;

mod connection;
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
    ) -> ports::l1::Result<Self> {
        let blob_signer = if let Some(key_arn) = blob_pool_key_arn {
            Some(aws_client.make_signer(key_arn).await?)
        } else {
            None
        };

        let main_signer = aws_client.make_signer(main_key_arn).await?;

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

    pub(crate) async fn submit(&self, block: ValidatedFuelBlock) -> Result<BlockSubmissionTx> {
        Ok(self.inner.submit(block).await?)
    }

    pub(crate) fn commit_interval(&self) -> NonZeroU32 {
        self.inner.commit_interval()
    }

    pub(crate) async fn get_block_number(&self) -> Result<u64> {
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
