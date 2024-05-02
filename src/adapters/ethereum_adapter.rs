mod monitored_adapter;
mod websocket;

use std::pin::Pin;

use async_trait::async_trait;
use ethers::types::{H160, U256};
use futures::Stream;
pub use monitored_adapter::MonitoredEthAdapter;
use rand::distributions::{Distribution, Standard};
pub use websocket::EthereumWs;

use crate::{
    adapters::fuel_adapter::FuelBlock,
    errors::{Error, Result},
};

#[derive(Clone, Copy)]
pub struct FuelBlockCommittedOnEth {
    pub fuel_block_hash: [u8; 32],
    pub commit_height: U256,
}

impl std::fmt::Debug for FuelBlockCommittedOnEth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hash = self
            .fuel_block_hash
            .map(|byte| format!("{byte:02x?}"))
            .join("");
        f.debug_struct("FuelBlockCommittedOnEth")
            .field("hash", &hash)
            .field("commit_height", &self.commit_height)
            .finish()
    }
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait EventStreamer {
    async fn establish_stream<'a>(
        &'a self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<FuelBlockCommittedOnEth>> + 'a + Send>>>;
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait EthereumAdapter: Send + Sync {
    async fn submit(&self, block: FuelBlock) -> Result<()>;
    async fn get_block_number(&self) -> Result<EthHeight>;
    async fn balance(&self, address: H160) -> Result<U256>;
    fn event_streamer(&self, eth_block_height: u64) -> Box<dyn EventStreamer + Send + Sync>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
pub struct EthHeight {
    height: i64,
}

impl Distribution<EthHeight> for Standard {
    fn sample<R: rand::prelude::Rng + ?Sized>(&self, rng: &mut R) -> EthHeight {
        let height: i64 = rng.gen_range(0..=i64::MAX);
        height.try_into().expect("Must be valid EthHeight")
    }
}

impl TryFrom<i64> for EthHeight {
    type Error = Error;

    fn try_from(height: i64) -> Result<Self> {
        if height < 0 {
            return Err(Error::Other(format!(
                "Height({height}) must be non-negative"
            )));
        }
        Ok(EthHeight { height })
    }
}

impl TryFrom<u64> for EthHeight {
    type Error = Error;
    fn try_from(height: u64) -> Result<Self> {
        if height >= i64::MAX as u64 {
            return Err(Error::Other(format!(
                "Height({height}) too large. DB can handle at most {}",
                i64::MAX
            )));
        }
        Ok(Self {
            height: height as i64,
        })
    }
}

impl From<u32> for EthHeight {
    fn from(height: u32) -> Self {
        Self {
            height: i64::from(height),
        }
    }
}

impl From<EthHeight> for i64 {
    fn from(height: EthHeight) -> Self {
        height.height
    }
}

impl From<EthHeight> for u64 {
    fn from(height: EthHeight) -> Self {
        height.height as u64
    }
}
