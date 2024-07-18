use ::metrics::{prometheus::core::Collector, HealthChecker, RegistersMetrics};
use ethers::types::{Address, Chain};
use ports::{
    l1::Result,
    types::{TransactionReceipt, ValidatedFuelBlock, U256},
};
use std::num::NonZeroU32;
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
        wallet_key: &str,
        blob_pool_wallet_key: Option<String>,
        unhealthy_after_n_errors: usize,
    ) -> ports::l1::Result<Self> {
        let provider = WsConnection::connect(
            url,
            chain_id,
            contract_address,
            wallet_key,
            blob_pool_wallet_key,
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

    pub(crate) async fn submit(&self, block: ValidatedFuelBlock) -> Result<()> {
        Ok(self.inner.submit(block).await?)
    }

    pub(crate) fn commit_interval(&self) -> NonZeroU32 {
        self.inner.commit_interval()
    }

    pub(crate) async fn get_block_number(&self) -> Result<u64> {
        Ok(self.inner.get_block_number().await?)
    }

    pub(crate) async fn get_transaction_receipt(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionReceipt>> {
        Ok(self.inner.get_transaction_receipt(tx_hash).await?)
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
