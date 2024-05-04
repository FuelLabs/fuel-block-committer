use std::num::NonZeroU32;

use ethers::types::{Address, Chain};
use metrics::{HealthChecker, RegistersMetrics};
use ports::{FuelBlock, U256};
use url::Url;

pub use self::event_streamer::EthEventStreamer;
use self::{
    connection::WsConnection,
    health_tracking_middleware::{HealthTrackingMiddleware, MyAdapter},
};
use crate::Result;

mod connection;
mod event_streamer;
mod health_tracking_middleware;

#[derive(Clone)]
pub struct WsAdapter {
    inner: HealthTrackingMiddleware<WsConnection>,
}

impl WsAdapter {
    pub async fn connect(
        ethereum_rpc: &Url,
        chain_id: Chain,
        contract_address: Address,
        ethereum_wallet_key: &str,
        commit_interval: NonZeroU32,
        unhealthy_after_n_errors: usize,
    ) -> Result<Self> {
        let provider = WsConnection::connect(
            ethereum_rpc,
            chain_id,
            contract_address,
            ethereum_wallet_key,
            commit_interval,
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

    pub(crate) async fn submit(&self, block: FuelBlock) -> Result<()> {
        self.inner.submit(block).await
    }

    pub(crate) async fn get_block_number(&self) -> Result<u64> {
        self.inner.get_block_number().await
    }

    pub(crate) async fn balance(&self) -> Result<U256> {
        self.inner.balance().await
    }

    #[cfg(feature = "test-helpers")]
    pub async fn finalized(&self, block: FuelBlock) -> Result<bool> {
        self.inner.finalized(block).await
    }

    #[cfg(feature = "test-helpers")]
    pub async fn block_hash_at_commit_height(&self, commit_height: u32) -> Result<[u8; 32]> {
        self.inner.block_hash_at_commit_height(commit_height).await
    }
}

// User responsible for registering any metrics T might have
impl RegistersMetrics for WsAdapter {
    fn metrics(&self) -> Vec<Box<dyn prometheus::core::Collector>> {
        self.inner.metrics()
    }
}
