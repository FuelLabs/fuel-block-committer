mod eth_test_adapter;

use std::time::Duration;

use anyhow::Result;

use crate::eth_test_adapter::FuelStateContract;

const FUEL_NODE_PORT: u16 = 4000;
const ETH_NODE_PORT: u16 = 8545;

#[tokio::test(flavor = "multi_thread")]
async fn submitted_correct_block_and_was_finalized() -> Result<()> {
    let fuel_node_address = format!("http://localhost:{FUEL_NODE_PORT}");
    let provider = fuels::accounts::provider::Provider::connect(&fuel_node_address)
        .await
        .unwrap();

    let fuel_contract = FuelStateContract::connect(ETH_NODE_PORT).await?;

    provider.produce_blocks(3, None).await?;

    // time enough to fwd the block to ethereum and for the TIME_TO_FINALIZE (1s) to elapse
    tokio::time::sleep(Duration::from_secs(5)).await;

    let latest_block = provider.chain_info().await?.latest_block;
    assert_eq!(latest_block.header.height, 3);

    assert!(
        fuel_contract
            .finalized(latest_block.id, latest_block.header.height)
            .await?
    );

    Ok(())
}
