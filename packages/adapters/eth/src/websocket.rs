use std::{num::NonZeroU32, time::Duration};

use ::metrics::{prometheus::core::Collector, HealthChecker, RegistersMetrics};
use alloy::{primitives::Address, signers::Signer};
use services::{
    types::{
        BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx, NonEmpty, TransactionResponse, U256,
    },
    Result,
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

#[derive(Debug, Clone)]
pub struct KmsKeys {
    pub main_key_arn: String,
    pub blob_pool_key_arn: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TxConfig {
    pub tx_max_fee: u128,
    pub send_tx_request_timeout: Duration,
}

impl WebsocketClient {
    pub async fn connect(
        url: Url,
        contract_address: Address,
        keys: KmsKeys,
        unhealthy_after_n_errors: usize,
        aws_client: AwsClient,
        tx_config: TxConfig,
    ) -> Result<Self> {
        let blob_signer = if let Some(key_arn) = keys.blob_pool_key_arn {
            Some(aws_client.make_signer(key_arn).await?)
        } else {
            None
        };

        let main_signer = aws_client.make_signer(keys.main_key_arn).await?;

        let blob_poster_address = blob_signer.as_ref().map(|signer| signer.address());
        let contract_caller_address = main_signer.address();

        let provider = WsConnection::connect(
            url,
            contract_address,
            main_signer,
            blob_signer,
            tx_config.tx_max_fee,
            tx_config.send_tx_request_timeout,
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

    pub(crate) async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> Result<bool> {
        Ok(self.inner.is_squeezed_out(tx_hash).await?)
    }

    pub(crate) async fn balance(&self, address: Address) -> Result<U256> {
        Ok(self.inner.balance(address).await?)
    }

    pub(crate) async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<services::types::L1Tx>,
    ) -> Result<(L1Tx, FragmentsSubmitted)> {
        Ok(self
            .inner
            .submit_state_fragments(fragments, previous_tx)
            .await?)
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
