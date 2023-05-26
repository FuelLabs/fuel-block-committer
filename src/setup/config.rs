use std::time::Duration;

use actix_web::dev::Url;
use fuels::{accounts::fuel_crypto::fuel_types::Bytes20, tx::Bytes32};

#[derive(Default, Debug, Clone)]
pub struct Config {
    pub ethereum_wallet_key: Bytes32,
    pub ethereum_rpc: Url,
    pub fuel_graphql_endpoint: Url,
    pub state_contract_address: Bytes20,
    pub commit_epoch: u32,
}

#[derive(Debug, Clone)]
pub struct ExtraConfig {
    pub fuel_polling_interval: Duration,
}

impl Default for ExtraConfig {
    fn default() -> Self {
        Self {
            fuel_polling_interval: Duration::from_secs(3),
        }
    }
}
