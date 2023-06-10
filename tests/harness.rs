mod eth_test_adapter;

use std::time::Duration;

use anyhow::Result;

use crate::eth_test_adapter::FuelStateContract;

#[tokio::test(flavor = "multi_thread")]
async fn will_commit_correct_block_at_the_correct_commit_height() -> Result<()> {
    let provider = fuels::accounts::provider::Provider::connect("http://localhost:4000")
        .await
        .unwrap();
    let fuel_contract = FuelStateContract::connect(8545).await?;

    provider.produce_blocks(3, None).await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let latest_block = provider.chain_info().await?.latest_block;

    assert_eq!(latest_block.header.height, 3);

    let block_hash = latest_block.id;
    assert_eq!(
        block_hash,
        fuel_contract.block_hash_at_commit_height(1).await?
    );

    Ok(())
}
