use std::{collections::BTreeMap, ops::RangeInclusive};

use nonempty::NonEmpty;
use services::{
    historical_fees::port::{
        l1::{testing, FeesProvider},
        service::HistoricalFeesProvider,
        BlockFees, Fees,
    },
    types::CollectNonEmpty,
};

#[tokio::test]
async fn calculates_sma_correctly_for_last_1_block() {
    // given
    let fees_provider = testing::TestFeesProvider::new(testing::incrementing_fees(5));
    let price_service = HistoricalFeesProvider::new(fees_provider);
    let last_n_blocks = 1;

    // when
    let sma = price_service.calculate_sma(last_n_blocks).await;

    // then
    assert_eq!(sma.base_fee_per_gas, 5);
    assert_eq!(sma.reward, 5);
    assert_eq!(sma.base_fee_per_blob_gas, 5);
}

#[tokio::test]
async fn calculates_sma_correctly_for_last_5_blocks() {
    // given
    let fees_provider = testing::TestFeesProvider::new(testing::incrementing_fees(5));
    let price_service = HistoricalFeesProvider::new(fees_provider);
    let last_n_blocks = 5;

    // when
    let sma = price_service.calculate_sma(last_n_blocks).await;

    // then
    let mean = (5 + 4 + 3 + 2 + 1) / 5;
    assert_eq!(sma.base_fee_per_gas, mean);
    assert_eq!(sma.reward, mean);
    assert_eq!(sma.base_fee_per_blob_gas, mean);
}
