use std::net::SocketAddr;

use fuels::{
    prelude::Provider,
    test_helpers::{setup_test_provider, Config},
};
use testcontainers::{core::WaitFor, *};
use tokio::process::Command;
#[derive(Debug, Default)]
pub struct EthNodeImage;

impl Image for EthNodeImage {
    type Args = ();

    fn name(&self) -> String {
        "eth_node".to_owned()
    }

    fn tag(&self) -> String {
        "latest".to_owned()
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stdout(
            "FuelERC20Gateway_impl: 0x58Bf5211B9A7176aeAFa7129516c4D7C83696951",
        )]
    }

    fn expose_ports(&self) -> Vec<u16> {
        vec![8545]
    }
}

pub struct FuelNode {
    pub provider: Provider,
    addr: SocketAddr,
}

impl FuelNode {
    pub async fn run() -> Self {
        let node_config = Config {
            manual_blocks_enabled: true,
            ..Config::local_node()
        };

        let (provider, addr) =
            setup_test_provider(vec![], vec![], Some(node_config), Some(Default::default())).await;

        Self { provider, addr }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }
}

pub fn run_committer(
    fuel_port: u16,
    eth_port: u16,
    commit_interval: usize,
) -> tokio::process::Child {
    let args = [
        "run",
        "--",
        &format!("--fuel-graphql-endpoint=http://127.0.0.1:{fuel_port}"),
        &format!("--ethereum-rpc=ws://127.0.0.1:{eth_port}"),
        "--ethereum-wallet-key=0x9e56ccf010fa4073274b8177ccaad46fbaf286645310d03ac9bb6afa922a7c36",
        "--state-contract-address=0xdAad669b06d79Cb48C8cfef789972436dBe6F24d",
        "--ethereum-chain=anvil",
        &format!("--commit-interval={commit_interval}"),
    ];

    Command::new(env!("CARGO")).args(&args).spawn().unwrap()
}
