#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Network(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait FuelAdapter: Send + Sync {
    async fn block_at_height(&self, height: u32) -> Result<Option<crate::FuelBlock>>;
    async fn latest_block(&self) -> Result<crate::FuelBlock>;
}
