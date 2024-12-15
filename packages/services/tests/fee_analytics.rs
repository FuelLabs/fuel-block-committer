use std::{collections::HashMap, path::PathBuf};

use eth::make_pub_eth_client;
use services::{
    fee_analytics::{
        self,
        port::{
            l1::testing::{self, TestFeesProvider},
            BlockFees, Fees,
        },
        service::FeeAnalytics,
    },
    state_committer::fee_optimization::{Context, Feethresholds, SendOrWaitDecider},
};

#[tokio::test]
async fn calculates_sma_correctly_for_last_1_block() {
    // given
    let fees_provider = testing::TestFeesProvider::new(testing::incrementing_fees(5));
    let fee_analytics = FeeAnalytics::new(fees_provider);
    let last_n_blocks = 1;

    // when
    let sma = fee_analytics.calculate_sma(4..=4).await;

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
    let sma = fee_analytics.calculate_sma(0..=4).await;

    // then
    let mean = (5 + 4 + 3 + 2 + 1) / 5;
    assert_eq!(sma.base_fee_per_gas, mean);
    assert_eq!(sma.reward, mean);
    assert_eq!(sma.base_fee_per_blob_gas, mean);
}

fn calculate_tx_fee(fees: &Fees) -> u128 {
    21_000 * fees.base_fee_per_gas + fees.reward + 6 * fees.base_fee_per_blob_gas * 131_072
}

fn save_tx_fees(tx_fees: &[(u64, u128)], path: &str) {
    let mut csv_writer =
        csv::Writer::from_path(PathBuf::from("/home/segfault_magnet/grafovi/").join(path)).unwrap();
    csv_writer
        .write_record(["height", "tx_fee"].iter())
        .unwrap();
    for (height, fee) in tx_fees {
        csv_writer
            .write_record([height.to_string(), fee.to_string()])
            .unwrap();
    }
    csv_writer.flush().unwrap();
}

// #[tokio::test]
// async fn something() {
//     let client = make_pub_eth_client().await;
//     use services::fee_analytics::port::l1::FeesProvider;
//
//     let current_block_height = 21408300;
//     let starting_block_height = current_block_height - 48 * 3600 / 12;
//     let data = client
//         .fees(starting_block_height..=current_block_height)
//         .await
//         .into_iter()
//         .collect::<Vec<_>>();
//
//     let fee_lookup = data
//         .iter()
//         .map(|b| (b.height, b.fees))
//         .collect::<HashMap<_, _>>();
//
//     let short_sma = 25u64;
//     let long_sma = 900;
//
//     let current_tx_fees = data
//         .iter()
//         .map(|b| (b.height, calculate_tx_fee(&b.fees)))
//         .collect::<Vec<_>>();
//
//     save_tx_fees(&current_tx_fees, "current_fees.csv");
//
//     let local_client = TestFeesProvider::new(data.clone().into_iter().map(|e| (e.height, e.fees)));
//     let fee_analytics = FeeAnalytics::new(local_client.clone());
//
//     let mut short_sma_tx_fees = vec![];
//     for height in (starting_block_height..=current_block_height).skip(short_sma as usize) {
//         let fees = fee_analytics
//             .calculate_sma(height - short_sma..=height)
//             .await;
//
//         let tx_fee = calculate_tx_fee(&fees);
//
//         short_sma_tx_fees.push((height, tx_fee));
//     }
//     save_tx_fees(&short_sma_tx_fees, "short_sma_fees.csv");
//
//     let decider = SendOrWaitDecider::new(
//         FeeAnalytics::new(local_client.clone()),
//         services::state_committer::fee_optimization::Config {
//             sma_periods: services::state_committer::fee_optimization::SmaBlockNumPeriods {
//                 short: short_sma,
//                 long: long_sma,
//             },
//             fee_thresholds: Feethresholds {
//                 max_l2_blocks_behind: 43200 * 3,
//                 start_discount_percentage: 0.2,
//                 end_premium_percentage: 0.2,
//                 always_acceptable_fee: 1000000000000000u128,
//             },
//         },
//     );
//
//     let mut decisions = vec![];
//     let mut long_sma_tx_fees = vec![];
//
//     for height in (starting_block_height..=current_block_height).skip(long_sma as usize) {
//         let fees = fee_analytics
//             .calculate_sma(height - long_sma..=height)
//             .await;
//         let tx_fee = calculate_tx_fee(&fees);
//         long_sma_tx_fees.push((height, tx_fee));
//
//         if decider
//             .should_send_blob_tx(
//                 6,
//             Context {
//                 at_l1_height: height,
//                 num_l2_blocks_behind: (height - starting_block_height) * 12,
//             },
//         )
//         .await
//     {
//         let current_fees = fee_lookup.get(&height).unwrap();
//         let current_tx_fee = calculate_tx_fee(current_fees);
//         decisions.push((height, current_tx_fee));
//     }
// }
//
// save_tx_fees(&long_sma_tx_fees, "long_sma_fees.csv");
// save_tx_fees(&decisions, "decisions.csv");
// }
