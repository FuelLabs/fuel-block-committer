use std::{net::SocketAddr, time::Duration};

use fuels::{
    prelude::Provider,
    test_helpers::{setup_test_provider, Config},
};
use testcontainers::{clients::Cli, core::WaitFor, *};
use tokio::process::Command;

#[tokio::test(flavor = "multi_thread")]
async fn cli_can_run_hello_world() {
    let docker = Cli::default();
    let container = docker.run(EthNodeImage);
    let eth_port = container.get_host_port_ipv4(8545);

    let fuel_node = FuelNode::run().await;
    let provider = &fuel_node.provider;

    let fuel_port = fuel_node.port();

    run_committer(fuel_port, eth_port);

    for height in 1..100 {
        eprintln!("producing block of height {height}");
        provider.produce_blocks(height, None).await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

#[derive(Debug, Default)]
pub struct EthNodeImage;

impl Image for EthNodeImage {
    type Args = ();

    fn name(&self) -> String {
        "doit".to_owned()
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

struct FuelNode {
    provider: Provider,
    addr: SocketAddr,
}

impl FuelNode {
    async fn run() -> Self {
        let node_config = Config {
            manual_blocks_enabled: true,
            ..Config::local_node()
        };

        let (provider, addr) =
            setup_test_provider(vec![], vec![], Some(node_config), Some(Default::default())).await;

        Self { provider, addr }
    }

    fn port(&self) -> u16 {
        self.addr.port()
    }
}

fn run_committer(fuel_port: u16, eth_port: u16) -> tokio::process::Child {
    let args = [
        "run",
        "--",
        &format!("--fuel-graphql-endpoint=http://127.0.0.1:{fuel_port}"),
        &format!("--ethereum-rpc=http://127.0.0.1:{eth_port}"),
        "--ethereum-wallet-key=0x9e56ccf010fa4073274b8177ccaad46fbaf286645310d03ac9bb6afa922a7c36",
        "--state-contract-address=0xdAad669b06d79Cb48C8cfef789972436dBe6F24d",
        "--ethereum-chain=anvil",
        "--commit-interval=1",
    ];

    Command::new(env!("CARGO")).args(&args).spawn().unwrap()
}
