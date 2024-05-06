#![deny(unused_crate_dependencies)]
mod block_committer;
mod block_watcher;
mod commit_listener;
mod health_reporter;
mod status_reporter;
mod wallet_balance_tracker;

pub use block_committer::BlockCommitter;
pub use block_watcher::BlockWatcher;
pub use commit_listener::CommitListener;
pub use health_reporter::HealthReporter;
pub use status_reporter::StatusReporter;
pub use wallet_balance_tracker::WalletBalanceTracker;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Other(String),
    #[error("Network Error: {0}")]
    Network(String),
    #[error("Storage error: {0}")]
    Storage(String),
}

impl From<ports::eth_rpc::Error> for Error {
    fn from(value: ports::eth_rpc::Error) -> Self {
        match value {
            ports::eth_rpc::Error::Network(e) => Self::Network(e),
            _ => Self::Other(value.to_string()),
        }
    }
}

impl From<ports::fuel_rpc::Error> for Error {
    fn from(value: ports::fuel_rpc::Error) -> Self {
        match value {
            ports::fuel_rpc::Error::Network(e) => Self::Network(e),
        }
    }
}

impl From<ports::storage::Error> for Error {
    fn from(error: ports::storage::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[async_trait::async_trait]
pub trait Runner: Send + Sync {
    async fn run(&mut self) -> Result<()>;
}
