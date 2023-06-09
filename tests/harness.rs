mod committer;
mod eth_node;
mod fuel_node;

use std::time::Duration;

use crate::{
    committer::run_committer,
    eth_node::{EthNode, EthNodeConfig},
    fuel_node::FuelNode,
};

#[tokio::test(flavor = "multi_thread")]
async fn cli_can_run_hello_world() {
    let eth_node = EthNode::run(EthNodeConfig {
        print_output: true,
        ..Default::default()
    });
    eth_node.wait_until_ready().await;

    let eth_port = eth_node.port();

    let fuel_node = FuelNode::run().await;
    let provider = &fuel_node.provider;
    let fuel_port = fuel_node.port();

    let _committer = run_committer(fuel_port, eth_port, 3);

    for height in 1..100 {
        eprintln!("producing block of height {height}");
        provider.produce_blocks(1, None).await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}
