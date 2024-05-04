use std::pin::Pin;

use types::InvalidEthHeight;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Network(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<InvalidEthHeight> for Error {
    fn from(err: InvalidEthHeight) -> Self {
        Self::Other(err.to_string())
    }
}

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait EthereumAdapter: Send + Sync {
    async fn submit(&self, block: crate::FuelBlock) -> Result<()>;
    async fn get_block_number(&self) -> Result<crate::EthHeight>;
    async fn balance(&self) -> Result<crate::U256>;
    fn event_streamer(&self, eth_block_height: u64) -> Box<dyn EventStreamer + Send + Sync>;
}

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait EventStreamer {
    async fn establish_stream<'a>(
        &'a self,
    ) -> Result<Pin<Box<dyn crate::Stream<Item = Result<FuelBlockCommittedOnEth>> + 'a + Send>>>;
}

#[derive(Clone, Copy)]
pub struct FuelBlockCommittedOnEth {
    pub fuel_block_hash: [u8; 32],
    pub commit_height: crate::U256,
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
