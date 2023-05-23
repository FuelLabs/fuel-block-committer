use fuel_block_committer_api::launch;
use lazy_static::lazy_static;
use prometheus::{labels, opts, register_gauge, Gauge};

#[tokio::main]
async fn main() {
    lazy_static! {
        static ref LATEST_FUEL_BLOCK: Gauge = register_gauge!(opts!(
            "latest_fuel_block",
            "The latest fuel block.",
            labels! {"handler" => "all",}
        ))
        .unwrap();
        static ref LATEST_COMMITED_BLOCK: Gauge = register_gauge!(opts!(
            "latest_committed_block",
            "The latest commited block.",
            labels! {"handler" => "all",}
        ))
        .unwrap();
        static ref ETHEREUM_WALLET_GAS_BALANCE: Gauge = register_gauge!(opts!(
            "ethereum_wallet_gas_balance",
            "The ethereum wallet gas balance",
            labels! {"handler" => "all",}
        ))
        .unwrap();
    }

    launch().await.unwrap();
}
