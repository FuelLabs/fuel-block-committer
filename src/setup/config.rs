use std::{net::Ipv4Addr, num::NonZeroU32, path::PathBuf, time::Duration};

use ethers::types::{Address, Chain};
use url::Url;

#[derive(Debug, Clone)]
pub struct Config {
    pub ethereum_wallet_key: String,
    pub ethereum_rpc: Url,
    pub ethereum_chain_id: Chain,
    pub fuel_graphql_endpoint: Url,
    pub state_contract_address: Address,
    pub commit_interval: NonZeroU32,
    pub port: u16,
    pub host: Ipv4Addr,
    pub db_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct InternalConfig {
    pub fuel_polling_interval: Duration,
    pub fuel_errors_before_unhealthy: usize,
    pub between_eth_event_stream_restablishing_attempts: Duration,
    pub eth_errors_before_unhealthy: usize,
}

impl Default for InternalConfig {
    fn default() -> Self {
        Self {
            fuel_polling_interval: Duration::from_secs(3),
            fuel_errors_before_unhealthy: 3,
            between_eth_event_stream_restablishing_attempts: Duration::from_secs(3),
            eth_errors_before_unhealthy: 3,
        }
    }
}
