mod committer;
mod eth_node;
mod eth_test_adapter;
mod fuel_node;

use std::time::Duration;

use crate::{
    committer::run_committer,
    eth_node::{EthNode, EthNodeConfig},
    eth_test_adapter::FuelStateContract,
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
    let fuel_contract = FuelStateContract::connect(eth_port).await;

    provider.produce_blocks(3, None).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let block_hash = provider.chain_info().await.unwrap().latest_block.id;
    eprintln!("Expecting block hash of: {block_hash}");
    assert_eq!(
        block_hash,
        fuel_contract.block_hash_at_commit_height(1).await
    );

    // validate finalized
}
