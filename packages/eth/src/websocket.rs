use std::num::NonZeroU32;

use ::metrics::{prometheus::core::Collector, HealthChecker, RegistersMetrics};
use alloy::{primitives::Address, signers::Signer};
use ports::{
    l1::{FragmentsSubmitted, Result},
    types::{BlockSubmissionTx, Fragment, NonEmpty, TransactionResponse, U256},
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
    blob_poster_address: Option<Address>,
    contract_caller_address: Address,
}

impl WebsocketClient {
    pub async fn connect(
        url: Url,
        contract_address: Address,
        main_key_arn: String,
        blob_pool_key_arn: Option<String>,
        unhealthy_after_n_errors: usize,
        aws_client: AwsClient,
        first_tx_gas_estimation_multiplier: Option<u64>,
    ) -> ports::l1::Result<Self> {
        let blob_signer = if let Some(key_arn) = blob_pool_key_arn {
            Some(aws_client.make_signer(key_arn).await?)
        } else {
            None
        };

        let main_signer = aws_client.make_signer(main_key_arn).await?;

        let blob_poster_address = blob_signer.as_ref().map(|signer| signer.address());
        let contract_caller_address = main_signer.address();

        let provider = WsConnection::connect(
            url,
            contract_address,
            main_signer,
            blob_signer,
            first_tx_gas_estimation_multiplier,
        )
        .await?;

        Ok(Self {
            inner: HealthTrackingMiddleware::new(provider, unhealthy_after_n_errors),
            blob_poster_address,
            contract_caller_address,
        })
    }

    #[must_use]
    pub fn connection_health_checker(&self) -> HealthChecker {
        self.inner.connection_health_checker()
    }

    pub(crate) async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx> {
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

    pub(crate) async fn is_in_mempool(&self, tx_hash: [u8; 32]) -> Result<bool> {
        Ok(self.inner.is_in_mempool(tx_hash).await?)
    }

    pub(crate) async fn balance(&self, address: Address) -> Result<U256> {
        Ok(self.inner.balance(address).await?)
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

    pub fn blob_poster_address(&self) -> Option<Address> {
        self.blob_poster_address
    }

    pub fn contract_caller_address(&self) -> Address {
        self.contract_caller_address
    }
}

// User responsible for registering any metrics T might have
impl RegistersMetrics for WebsocketClient {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        self.inner.metrics()
    }
}
