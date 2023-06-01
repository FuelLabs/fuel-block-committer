mod utils;
use std::time::Duration;

use testcontainers::clients::Cli;

use crate::utils::{run_committer, EthNodeImage, FuelNode};

#[tokio::test(flavor = "multi_thread")]
async fn cli_can_run_hello_world() {
    let docker = Cli::default();
    let container = docker.run(EthNodeImage);
    let eth_port = container.get_host_port_ipv4(8545);

    let fuel_node = FuelNode::run().await;
    let provider = &fuel_node.provider;
    let fuel_port = fuel_node.port();

    run_committer(fuel_port, eth_port, 3);

    for height in 1..100 {
        eprintln!("producing block of height {height}");
        provider.produce_blocks(height, None).await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}
