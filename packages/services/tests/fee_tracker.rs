use services::fee_tracker::port::l1::testing::PreconfiguredFeeApi;
use services::fee_tracker::port::l1::Api;
use services::fee_tracker::service::FeeThresholds;
use services::fee_tracker::service::FeeTracker;
use services::fee_tracker::service::Percentage;
use services::fee_tracker::service::SmaPeriods;
use services::fee_tracker::{port::l1::Fees, service::Config};
use services::state_committer::service::SendOrWaitDecider;
use test_case::test_case;

fn generate_fees(config: Config, old_fees: Fees, new_fees: Fees) -> Vec<(u64, Fees)> {
    let older_fees = std::iter::repeat_n(
        old_fees,
        (config.sma_periods.long.get() - config.sma_periods.short.get()) as usize,
    );
    let newer_fees = std::iter::repeat_n(new_fees, config.sma_periods.short.get() as usize);

    older_fees
        .chain(newer_fees)
        .enumerate()
        .map(|(i, f)| (i as u64, f))
        .collect()
}

#[test_case(
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        6,
        Config {
            sma_periods: services::fee_tracker::service::SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            },
        },
        0, // not behind at all
        true;
        "Should send because all short-term fees are lower than long-term"
    )]
#[test_case(
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            },
        },
        0,
        false;
        "Should not send because all short-term fees are higher than long-term"
    )]
#[test_case(
        Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                always_acceptable_fee: (21_000 * 5000) + (6 * 131_072 * 5000) + 5000 + 1,
                max_l2_blocks_behind: 100.try_into().unwrap(),
                ..Default::default()
            }
        },
        0,
        true;
        "Should send since short-term fee < always_acceptable_fee"
    )]
#[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 1500, reward: 10000, base_fee_per_blob_gas: 1000 },
        5,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Should send because short-term base_fee_per_gas is lower"
    )]
#[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2500, reward: 10000, base_fee_per_blob_gas: 1000 },
        5,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        false;
        "Should not send because short-term base_fee_per_gas is higher"
    )]
#[test_case(
        Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 900 },
        5,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Should send because short-term base_fee_per_blob_gas is lower"
    )]
#[test_case(
        Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1100 },
        5,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        false;
        "Should not send because short-term base_fee_per_blob_gas is higher"
    )]
#[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 9000, base_fee_per_blob_gas: 1000 },
        5,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Should send because short-term reward is lower"
    )]
#[test_case(
        Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000 },
        Fees { base_fee_per_gas: 2000, reward: 11000, base_fee_per_blob_gas: 1000 },
        5,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        false;
        "Should not send because short-term reward is higher"
    )]
#[test_case(
        // Multiple short-term fees are lower
        Fees { base_fee_per_gas: 4000, reward: 8000, base_fee_per_blob_gas: 4000 },
        Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 3500 },
        6,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Should send because multiple short-term fees are lower"
    )]
#[test_case(
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000 },
        6,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        false;
        "Should not send because all fees are identical and no tolerance"
    )]
#[test_case(
        // Zero blobs scenario: blob fee differences don't matter
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 2500, reward: 5500, base_fee_per_blob_gas: 5000 },
        0,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Zero blobs: short-term base_fee_per_gas and reward are lower, send"
    )]
#[test_case(
        // Zero blobs but short-term reward is higher
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 5000 },
        0,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        false;
        "Zero blobs: short-term reward is higher, don't send"
    )]
#[test_case(
        // Zero blobs don't care about higher short-term base_fee_per_blob_gas
        Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000 },
        Fees { base_fee_per_gas: 2000, reward: 7000, base_fee_per_blob_gas: 50_000_000 },
        0,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Zero blobs: ignore blob fee, short-term base_fee_per_gas is lower, send"
    )]
// Initially not send, but as num_l2_blocks_behind increases, acceptance grows.
#[test_case(
        // Initially short-term fee too high compared to long-term (strict scenario), no send at t=0
    Fees { base_fee_per_gas: 6000, reward: 0, base_fee_per_blob_gas: 6000 },
    Fees { base_fee_per_gas: 7000, reward: 0, base_fee_per_blob_gas: 7000 },
        1,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                start_discount_percentage: Percentage::try_from(0.20).unwrap(),
                end_premium_percentage: Percentage::try_from(0.20).unwrap(),
                always_acceptable_fee: 0,
            },
        },
        0,
        false;
        "Early: short-term expensive, not send"
    )]
#[test_case(
        // At max_l2_blocks_behind, send regardless
        Fees { base_fee_per_gas: 6000, reward: 0, base_fee_per_blob_gas: 6000 },
        Fees { base_fee_per_gas: 7000, reward: 0, base_fee_per_blob_gas: 7000 },
        1,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                start_discount_percentage: 0.20.try_into().unwrap(),
                end_premium_percentage: 0.20.try_into().unwrap(),
                always_acceptable_fee: 0,
            }
        },
        100,
        true;
        "Later: after max wait, send regardless"
    )]
#[test_case(
        Fees { base_fee_per_gas: 6000, reward: 0, base_fee_per_blob_gas: 6000 },
        Fees { base_fee_per_gas: 7000, reward: 0, base_fee_per_blob_gas: 7000 },
        1,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                start_discount_percentage: 0.20.try_into().unwrap(),
                end_premium_percentage: 0.20.try_into().unwrap(),
                always_acceptable_fee: 0,
            },
        },
        80,
        true;
        "Mid-wait: increased tolerance allows acceptance"
    )]
#[test_case(
        // Short-term fee is huge, but always_acceptable_fee is large, so send immediately
        Fees { base_fee_per_gas: 100_000, reward: 0, base_fee_per_blob_gas: 100_000 },
        Fees { base_fee_per_gas: 2_000_000, reward: 1_000_000, base_fee_per_blob_gas: 20_000_000 },
        1,
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                start_discount_percentage: 0.20.try_into().unwrap(),
                end_premium_percentage: 0.20.try_into().unwrap(),
                always_acceptable_fee: 2_700_000_000_000
            },
        },
        0,
        true;
        "Always acceptable fee triggers immediate send"
    )]
#[tokio::test]
async fn parameterized_send_or_wait_tests(
    old_fees: Fees,
    new_fees: Fees,
    num_blobs: u32,
    config: Config,
    num_l2_blocks_behind: u32,
    expected_decision: bool,
) {
    let fees = generate_fees(config, old_fees, new_fees);
    let fees_provider = PreconfiguredFeeApi::new(fees);
    let current_block_height = fees_provider.current_height().await.unwrap();

    let sut = FeeTracker::new(fees_provider, config);

    let should_send = sut
        .should_send_blob_tx(num_blobs, num_l2_blocks_behind, current_block_height)
        .await
        .unwrap();

    assert_eq!(
            should_send, expected_decision,
            "For num_blobs={num_blobs}, num_l2_blocks_behind={num_l2_blocks_behind}, config={config:?}: Expected decision: {expected_decision}, got: {should_send}",
        );
}

#[tokio::test]
async fn test_send_when_too_far_behind_and_fee_provider_fails() {
    // given
    let config = Config {
        sma_periods: SmaPeriods {
            short: 2.try_into().unwrap(),
            long: 6.try_into().unwrap(),
        },
        fee_thresholds: FeeThresholds {
            max_l2_blocks_behind: 10.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
    };

    // having no fees will make the validation in fee analytics fail
    let fee_provider = PreconfiguredFeeApi::new(vec![]);
    let sut = FeeTracker::new(fee_provider, config);

    // when
    let should_send = sut
        .should_send_blob_tx(1, 20, 100)
        .await
        .expect("Should send despite fee provider failure");

    // then
    assert!(
        should_send,
        "Should send because too far behind, regardless of fee provider status"
    );
}
