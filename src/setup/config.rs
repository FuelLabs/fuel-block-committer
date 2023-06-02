use std::{net::Ipv4Addr, path::PathBuf, time::Duration};

use fuels::{accounts::fuel_crypto::fuel_types::Bytes20, tx::Bytes32};
use url::Url;

#[derive(Debug, Clone)]
pub struct Config {
    pub ethereum_wallet_key: Bytes32,
    pub ethereum_rpc: Url,
    pub fuel_graphql_endpoint: Url,
    pub state_contract_address: Bytes20,
    pub commit_epoch: u32,
    pub port: u16,
    pub host: Ipv4Addr,
    pub db_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct InternalConfig {
    pub fuel_polling_interval: Duration,
    pub fuel_errors_before_unhealthy: usize,
}

impl Default for InternalConfig {
    fn default() -> Self {
        Self {
            fuel_polling_interval: Duration::from_secs(3),
            fuel_errors_before_unhealthy: 3,
        }
    }
}
