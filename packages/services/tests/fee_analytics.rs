use services::fee_analytics::port::{l1::testing, service::FeeAnalytics};

#[tokio::test]
async fn calculates_sma_correctly_for_last_1_block() {
    // given
    let fees_provider = testing::TestFeesProvider::new(testing::incrementing_fees(5));
    let fee_analytics = FeeAnalytics::new(fees_provider);
    let last_n_blocks = 1;

    // when
    let sma = fee_analytics.calculate_sma(last_n_blocks).await;

    // then
    assert_eq!(sma.base_fee_per_gas, 5);
    assert_eq!(sma.reward, 5);
    assert_eq!(sma.base_fee_per_blob_gas, 5);
}

#[tokio::test]
async fn calculates_sma_correctly_for_last_5_blocks() {
    // given
    let fees_provider = testing::TestFeesProvider::new(testing::incrementing_fees(5));
    let fee_analytics = FeeAnalytics::new(fees_provider);
    let last_n_blocks = 5;

    // when
    let sma = fee_analytics.calculate_sma(last_n_blocks).await;

    // then
    let mean = (5 + 4 + 3 + 2 + 1) / 5;
    assert_eq!(sma.base_fee_per_gas, mean);
    assert_eq!(sma.reward, mean);
    assert_eq!(sma.base_fee_per_blob_gas, mean);
}
