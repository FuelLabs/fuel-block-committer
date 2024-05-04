pub mod postgresql;

use std::sync::Arc;

use async_trait::async_trait;
use rand::distributions::{Distribution, Standard};

use crate::adapters::{ethereum_adapter::EthHeight, fuel_adapter::FuelBlock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSubmission {
    pub block: FuelBlock,
    pub completed: bool,
    // Eth block height moments before submitting the fuel block. Used to filter stale events in
    // the commit listener.
    pub submittal_height: EthHeight,
}

impl Distribution<BlockSubmission> for Standard {
    fn sample<R: rand::Rng + ?Sized>(&self, rng: &mut R) -> BlockSubmission {
        BlockSubmission {
            block: rng.gen(),
            completed: rng.gen(),
            submittal_height: rng.gen(),
        }
    }
}

#[async_trait]
#[impl_tools::autoimpl(for<T: trait> &T, &mut T, Arc<T>, Box<T>)]
pub trait Storage: Send + Sync {
    async fn insert(&self, submission: BlockSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission>;
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Database Error {0}")]
    Database(String),
    #[error("Could not convert to/from domain/db type {0}")]
    Conversion(String),
    #[error("{0}")]
    Other(String),
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Self::Database(e.to_string())
    }
}

impl From<sqlx::migrate::MigrateError> for Error {
    fn from(e: sqlx::migrate::MigrateError) -> Self {
        Self::Database(e.to_string())
    }
}
