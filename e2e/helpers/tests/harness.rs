use std::sync::Arc;

use anyhow::Result;
use e2e_helpers::fuel_node_simulated::{FuelNode, SimulationConfig};
use fuel::HttpClient;
use tokio::sync::Mutex;

#[tokio::test]
async fn simulated_fuel_node_can_reply_to_adapter() -> Result<()> {
    let mut simulated_node = FuelNode::new(4000, Arc::new(Mutex::new(SimulationConfig::default())));
    simulated_node.run().await?;

    let client = HttpClient::new(&simulated_node.url(), 100, 1.try_into().unwrap());

    let latest_block = client.latest_block().await?;

    let block_at_height = client
        .block_at_height(latest_block.height)
        .await?
        .expect("block to exist");
    assert_eq!(latest_block, block_at_height);

    let da_compressed_block = client
        .compressed_block_at_height(block_at_height.height)
        .await?
        .expect("block to exist");
    assert_eq!(da_compressed_block.height, block_at_height.height);

    Ok(())
}

