pub mod block_bundler;
mod block_committer;
pub mod block_importer;
pub mod health_reporter;
mod state_committer;
pub mod state_listener;
pub mod state_pruner;
pub mod status_reporter;
pub mod wallet_balance_tracker;

pub mod ports {
    #[cfg(feature = "l1")]
    pub mod l1;

    #[cfg(feature = "fuel")]
    pub mod fuel;

    #[cfg(feature = "storage")]
    pub mod storage;

    #[cfg(feature = "clock")]
    pub mod clock;
}

#[cfg(any(
    feature = "l1",
    feature = "fuel",
    feature = "storage",
    feature = "clock"
))]
pub mod types;

pub use block_bundler::{
    bundler::Factory as BundlerFactory, service::BlockBundler,
    service::Config as BlockBundlerConfig,
};
#[cfg(feature = "test-helpers")]
pub use block_bundler::{
    bundler::{Bundle, BundleProposal, Bundler, Metadata},
    test_helpers::ControllableBundlerFactory,
};
pub use block_committer::BlockCommitter;
pub use state_committer::{Config as StateCommitterConfig, StateCommitter};
use types::InvalidL1Height;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Other(String),
    #[error("Network error: {0}")]
    Network(String),
    #[error("Storage error: {0}")]
    Storage(String),
    #[error("Block validation error: {0}")]
    BlockValidation(String),
}

impl From<InvalidL1Height> for Error {
    fn from(err: InvalidL1Height) -> Self {
        Self::Other(err.to_string())
    }
}

impl From<String> for Error {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[trait_variant::make(Send)]
pub trait Runner: Send + Sync {
    async fn run(&mut self) -> Result<()>;
}
