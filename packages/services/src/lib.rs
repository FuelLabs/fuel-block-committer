mod block_bundler;
mod block_committer;
mod block_importer;
mod health_reporter;
mod state_committer;
mod state_listener;
pub mod state_pruner;
pub mod status_reporter;
mod wallet_balance_tracker;

pub use block_bundler::{
    bundler::Factory as BundlerFactory, BlockBundler, Config as BlockBundlerConfig,
};
#[cfg(feature = "test-helpers")]
pub use block_bundler::{
    bundler::{Bundle, BundleProposal, Bundler, Metadata},
    ControllableBundlerFactory,
};
pub use block_committer::BlockCommitter;
pub use block_importer::BlockImporter;
pub use health_reporter::HealthReporter;
pub use state_committer::{Config as StateCommitterConfig, StateCommitter};
pub use state_listener::StateListener;
pub use wallet_balance_tracker::WalletBalanceTracker;

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

impl From<ports::l1::Error> for Error {
    fn from(error: ports::l1::Error) -> Self {
        match error {
            ports::l1::Error::Network(e) => Self::Network(e),
            _ => Self::Other(error.to_string()),
        }
    }
}

impl From<ports::fuel::Error> for Error {
    fn from(error: ports::fuel::Error) -> Self {
        match error {
            ports::fuel::Error::Network(e) => Self::Network(e),
            ports::fuel::Error::Other(e) => Self::Other(e.to_string()),
        }
    }
}

impl From<ports::storage::Error> for Error {
    fn from(error: ports::storage::Error) -> Self {
        Self::Storage(error.to_string())
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
