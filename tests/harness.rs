mod eth_test_adapter;

use std::time::Duration;

use anyhow::Result;

use crate::eth_test_adapter::FuelStateContract;

#[tokio::test(flavor = "multi_thread")]
async fn submitted_correct_block_and_was_finalized() -> Result<()> {
    let provider = fuels::accounts::provider::Provider::connect("http://localhost:4000")
        .await
        .unwrap();
    let fuel_contract = FuelStateContract::connect(8545).await?;

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
