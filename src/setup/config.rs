use std::{net::Ipv4Addr, time::Duration};

use fuels::{accounts::fuel_crypto::fuel_types::Bytes20};
use url::Url;

#[derive(Debug, Clone)]
pub struct Config {
    pub ethereum_wallet_key: String,
    pub ethereum_rpc: Url,
    pub fuel_graphql_endpoint: Url,
    pub state_contract_address: Bytes20,
    pub commit_epoch: u32,
    pub port: u16,
    pub host: Ipv4Addr,
}

#[derive(Debug, Clone)]
pub struct InternalConfig {
    pub fuel_polling_interval: Duration,
    pub eth_polling_interval: Duration,
    pub fuel_errors_before_unhealthy: usize,
}

impl Default for InternalConfig {
    fn default() -> Self {
        Self {
            fuel_polling_interval: Duration::from_secs(3),
            eth_polling_interval: Duration::from_secs(1),
            fuel_errors_before_unhealthy: 3,
        }
    }
}
