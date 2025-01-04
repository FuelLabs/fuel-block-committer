use std::time::Duration;

use metrics::prometheus;
use metrics::RegistersMetrics;
use services::{
    fees::Fees,
    state_committer::{AlgoConfig, FeeThresholds, SmaPeriods},
    types::{L1Tx, NonEmpty},
    Result, Runner, StateCommitter, StateCommitterConfig,
};
use test_helpers::{mocks, noop_fees, preconfigured_fees};

#[tokio::test]
async fn submits_fragments_when_required_count_accumulated() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let fragments = setup.insert_fragments(0, 4).await;

    let tx_hash = [0; 32];
    let mut l1_mock_submit = test_helpers::mocks::l1::expects_state_submissions([(
        Some(NonEmpty::from_vec(fragments.clone()).unwrap()),
        L1Tx {
            hash: tx_hash,
            nonce: 0,
            ..Default::default()
        },
    )]);
    l1_mock_submit
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(0) }));

    let fuel_mock = test_helpers::mocks::fuel::latest_height_is(0);
    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 4.try_into().unwrap(),
            ..Default::default()
        },
        setup.test_clock(),
        noop_fees(),
    );

    // when
    state_committer.run().await?;

    // then
    // Mocks validate that the fragments have been sent
    Ok(())
}

#[tokio::test]
async fn submits_fragments_on_timeout_before_accumulation() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    let fragments = setup.insert_fragments(0, 5).await; // Only 5 fragments, less than required

    let tx_hash = [1; 32];
    let mut l1_mock_submit = test_helpers::mocks::l1::expects_state_submissions([(
        Some(NonEmpty::from_vec(fragments.clone()).unwrap()),
        L1Tx {
            hash: tx_hash,
            nonce: 0,
            ..Default::default()
        },
    )]);

    l1_mock_submit
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(0) }));
    let fuel_mock = test_helpers::mocks::fuel::latest_height_is(0);
    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 10.try_into().unwrap(),
            ..Default::default()
        },
        test_clock.clone(),
        noop_fees(),
    );

    // Advance time beyond the timeout
    test_clock.advance_time(Duration::from_secs(61));

    // when
    state_committer.run().await?;

    // then
    // Mocks validate that the fragments have been sent despite insufficient accumulation
    Ok(())
}

#[tokio::test]
async fn does_not_submit_fragments_before_required_count_or_timeout() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    let _fragments = setup.insert_fragments(0, 5).await; // Only 5 fragments, less than required

    let l1_mock_submit = test_helpers::mocks::l1::expects_state_submissions([]); // Expect no submissions

    let fuel_mock = test_helpers::mocks::fuel::latest_height_is(0);
    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 10.try_into().unwrap(),
            ..Default::default()
        },
        test_clock.clone(),
        noop_fees(),
    );

    // Advance time less than the timeout
    test_clock.advance_time(Duration::from_secs(30));

    // when
    state_committer.run().await?;

    // then
    // Mocks validate that no fragments have been sent
    Ok(())
}

#[tokio::test]
async fn submits_fragments_when_required_count_before_timeout() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let fragments = setup.insert_fragments(0, 5).await;

    let tx_hash = [3; 32];
    let mut l1_mock_submit = test_helpers::mocks::l1::expects_state_submissions([(
        Some(NonEmpty::from_vec(fragments).unwrap()),
        L1Tx {
            hash: tx_hash,
            nonce: 0,
            ..Default::default()
        },
    )]);
    l1_mock_submit
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(0) }));

    let fuel_mock = test_helpers::mocks::fuel::latest_height_is(0);
    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 5.try_into().unwrap(),
            ..Default::default()
        },
        setup.test_clock(),
        noop_fees(),
    );

    // when
    state_committer.run().await?;

    // then
    // Mocks validate that the fragments have been sent
    Ok(())
}

#[tokio::test]
async fn timeout_measured_from_last_finalized_fragment() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    // Insert initial fragments
    setup.commit_single_block_bundle().await;

    let fragments_to_submit = setup.insert_fragments(1, 2).await;

    let tx_hash = [4; 32];
    let mut l1_mock_submit = test_helpers::mocks::l1::expects_state_submissions([(
        Some(NonEmpty::from_vec(fragments_to_submit).unwrap()),
        L1Tx {
            hash: tx_hash,
            nonce: 0,
            ..Default::default()
        },
    )]);
    l1_mock_submit
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(1) }));

    let fuel_mock = test_helpers::mocks::fuel::latest_height_is(1);
    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 10.try_into().unwrap(),
            ..Default::default()
        },
        test_clock.clone(),
        noop_fees(),
    );

    // Advance time to exceed the timeout since last finalized fragment
    test_clock.advance_time(Duration::from_secs(60));

    // when
    state_committer.run().await?;

    // then
    // Mocks validate that the fragments were sent even though the accumulation target was not reached
    Ok(())
}

#[tokio::test]
async fn timeout_measured_from_startup_if_no_finalized_fragment() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    let fragments = setup.insert_fragments(0, 5).await; // Only 5 fragments, less than required

    let tx_hash = [5; 32];
    let mut l1_mock_submit = test_helpers::mocks::l1::expects_state_submissions([(
        Some(NonEmpty::from_vec(fragments.clone()).unwrap()),
        L1Tx {
            hash: tx_hash,
            nonce: 0,
            ..Default::default()
        },
    )]);

    let fuel_mock = test_helpers::mocks::fuel::latest_height_is(0);
    l1_mock_submit
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(1) }));
    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 10.try_into().unwrap(),
            ..Default::default()
        },
        test_clock.clone(),
        noop_fees(),
    );

    // Advance time beyond the timeout from startup
    test_clock.advance_time(Duration::from_secs(61));

    // when
    state_committer.run().await?;

    // then
    // Mocks validate that the fragments have been sent despite insufficient accumulation
    Ok(())
}

#[tokio::test]
async fn resubmits_fragments_when_gas_bump_timeout_exceeded() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    let fragments = setup.insert_fragments(0, 5).await;

    let tx_hash_1 = [6; 32];
    let tx_hash_2 = [7; 32];
    let mut l1_mock_submit = test_helpers::mocks::l1::expects_state_submissions([
        (
            Some(NonEmpty::from_vec(fragments.clone()).unwrap()),
            L1Tx {
                hash: tx_hash_1,
                nonce: 0,
                ..Default::default()
            },
        ),
        (
            Some(NonEmpty::from_vec(fragments.clone()).unwrap()),
            L1Tx {
                hash: tx_hash_2,
                nonce: 1,
                ..Default::default()
            },
        ),
    ]);

    l1_mock_submit
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(0) }));

    let fuel_mock = test_helpers::mocks::fuel::latest_height_is(0);
    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 5.try_into().unwrap(),
            gas_bump_timeout: Duration::from_secs(60),
            ..Default::default()
        },
        test_clock.clone(),
        noop_fees(),
    );

    // Submit the initial fragments
    state_committer.run().await?;

    // Advance time beyond the gas bump timeout, we need to advance a bit more
    // because the db clock record the transaction
    test_clock.advance_time(Duration::from_secs(80));

    // when
    state_committer.run().await?;

    // then
    // Mocks validate that the fragments have been sent again
    Ok(())
}

#[tokio::test]
async fn sends_transaction_when_short_term_fee_favorable() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let expensive = Fees {
        base_fee_per_gas: 5000.try_into().unwrap(),
        reward: 5000.try_into().unwrap(),
        base_fee_per_blob_gas: 5000.try_into().unwrap(),
    };
    let cheap = Fees {
        base_fee_per_gas: 3000.try_into().unwrap(),
        reward: 3000.try_into().unwrap(),
        base_fee_per_blob_gas: 3000.try_into().unwrap(),
    };
    let fee_sequence = vec![
        (0, expensive),
        (1, expensive),
        (2, expensive),
        (3, expensive),
        (4, cheap),
        (5, cheap),
    ];

    let config = AlgoConfig {
        sma_periods: SmaPeriods {
            short: 2.try_into().unwrap(),
            long: 6.try_into().unwrap(),
        },
        fee_thresholds: FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
    };

    // enough fragments to meet the accumulation threshold
    let fragments = setup.insert_fragments(0, 6).await;

    let mut l1_mock_submit = test_helpers::mocks::l1::expects_state_submissions([(
        Some(NonEmpty::from_vec(fragments.clone()).unwrap()),
        L1Tx {
            hash: [0; 32],
            nonce: 0,
            ..Default::default()
        },
    )]);
    l1_mock_submit
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(5) }));

    let fuel_mock = test_helpers::mocks::fuel::latest_height_is(100);
    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 6.try_into().unwrap(),
            fee_algo: config,
            ..Default::default()
        },
        setup.test_clock(),
        preconfigured_fees(fee_sequence),
    );

    // when
    state_committer.run().await?;

    // then
    // mocks validate that the fragments have been sent
    Ok(())
}

#[tokio::test]
async fn does_not_send_transaction_when_short_term_fee_unfavorable() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let expensive = Fees {
        base_fee_per_gas: 5000.try_into().unwrap(),
        reward: 5000.try_into().unwrap(),
        base_fee_per_blob_gas: 5000.try_into().unwrap(),
    };
    let cheap = Fees {
        base_fee_per_gas: 3000.try_into().unwrap(),
        reward: 3000.try_into().unwrap(),
        base_fee_per_blob_gas: 3000.try_into().unwrap(),
    };

    let fee_sequence = vec![
        (0, cheap),
        (1, cheap),
        (2, cheap),
        (3, cheap),
        (4, expensive),
        (5, expensive),
    ];

    let fee_algo = AlgoConfig {
        sma_periods: SmaPeriods {
            short: 2.try_into().unwrap(),
            long: 6.try_into().unwrap(),
        },
        fee_thresholds: FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
    };

    // enough fragments to meet the accumulation threshold
    let _fragments = setup.insert_fragments(0, 6).await;

    let mut l1_mock = test_helpers::mocks::l1::expects_state_submissions([]);
    l1_mock
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(5) }));

    let fuel_mock = test_helpers::mocks::fuel::latest_height_is(6);
    let mut state_committer = StateCommitter::new(
        l1_mock,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 6.try_into().unwrap(),
            fee_algo,
            ..Default::default()
        },
        setup.test_clock(),
        preconfigured_fees(fee_sequence),
    );

    // when
    state_committer.run().await?;

    // then
    // mocks validate that no fragments have been sent
    Ok(())
}

#[tokio::test]
async fn sends_transaction_when_l2_blocks_behind_exceeds_max() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    // Define fee sequence with high fees to ensure that without the behind condition, it wouldn't send
    let fee_sequence = vec![
        (
            0,
            Fees {
                base_fee_per_gas: 7000.try_into().unwrap(),
                reward: 7000.try_into().unwrap(),
                base_fee_per_blob_gas: 7000.try_into().unwrap(),
            },
        ),
        (
            1,
            Fees {
                base_fee_per_gas: 7000.try_into().unwrap(),
                reward: 7000.try_into().unwrap(),
                base_fee_per_blob_gas: 7000.try_into().unwrap(),
            },
        ),
        (
            2,
            Fees {
                base_fee_per_gas: 7000.try_into().unwrap(),
                reward: 7000.try_into().unwrap(),
                base_fee_per_blob_gas: 7000.try_into().unwrap(),
            },
        ),
        (
            3,
            Fees {
                base_fee_per_gas: 7000.try_into().unwrap(),
                reward: 7000.try_into().unwrap(),
                base_fee_per_blob_gas: 7000.try_into().unwrap(),
            },
        ),
        (
            4,
            Fees {
                base_fee_per_gas: 7000.try_into().unwrap(),
                reward: 7000.try_into().unwrap(),
                base_fee_per_blob_gas: 7000.try_into().unwrap(),
            },
        ),
        (
            5,
            Fees {
                base_fee_per_gas: 7000.try_into().unwrap(),
                reward: 7000.try_into().unwrap(),
                base_fee_per_blob_gas: 7000.try_into().unwrap(),
            },
        ),
    ];

    let fee_algo = AlgoConfig {
        sma_periods: SmaPeriods {
            short: 2.try_into().unwrap(),
            long: 6.try_into().unwrap(),
        },
        fee_thresholds: FeeThresholds {
            max_l2_blocks_behind: 50.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
    };

    // Insert enough fragments to meet the accumulation threshold
    let fragments = setup.insert_fragments(0, 6).await;

    // Expect a state submission despite high fees because blocks behind exceed max
    let tx_hash = [0; 32];
    let mut l1_mock_submit = test_helpers::mocks::l1::expects_state_submissions([(
        Some(NonEmpty::from_vec(fragments.clone()).unwrap()),
        L1Tx {
            hash: tx_hash,
            nonce: 0,
            ..Default::default()
        },
    )]);
    l1_mock_submit
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(5) }));

    let fuel_mock = test_helpers::mocks::fuel::latest_height_is(50); // L2 height is 50, behind by 50
    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 6.try_into().unwrap(),
            fee_algo,
            ..Default::default()
        },
        setup.test_clock(),
        preconfigured_fees(fee_sequence),
    );

    // when
    state_committer.run().await?;

    // then
    // Mocks validate that the fragments have been sent despite high fees
    Ok(())
}

#[tokio::test]
async fn sends_transaction_when_nearing_max_blocks_behind_with_increased_tolerance() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let fee_sequence = vec![
        (
            95,
            Fees {
                base_fee_per_gas: 5000.try_into().unwrap(),
                reward: 5000.try_into().unwrap(),
                base_fee_per_blob_gas: 5000.try_into().unwrap(),
            },
        ),
        (
            96,
            Fees {
                base_fee_per_gas: 5000.try_into().unwrap(),
                reward: 5000.try_into().unwrap(),
                base_fee_per_blob_gas: 5000.try_into().unwrap(),
            },
        ),
        (
            97,
            Fees {
                base_fee_per_gas: 5000.try_into().unwrap(),
                reward: 5000.try_into().unwrap(),
                base_fee_per_blob_gas: 5000.try_into().unwrap(),
            },
        ),
        (
            98,
            Fees {
                base_fee_per_gas: 5000.try_into().unwrap(),
                reward: 5000.try_into().unwrap(),
                base_fee_per_blob_gas: 5000.try_into().unwrap(),
            },
        ),
        (
            99,
            Fees {
                base_fee_per_gas: 5800.try_into().unwrap(),
                reward: 5800.try_into().unwrap(),
                base_fee_per_blob_gas: 5800.try_into().unwrap(),
            },
        ),
        (
            100,
            Fees {
                base_fee_per_gas: 5800.try_into().unwrap(),
                reward: 5800.try_into().unwrap(),
                base_fee_per_blob_gas: 5800.try_into().unwrap(),
            },
        ),
    ];

    let fee_algo = AlgoConfig {
        sma_periods: SmaPeriods {
            short: 2.try_into().unwrap(),
            long: 5.try_into().unwrap(),
        },
        fee_thresholds: FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            multiplier_range: services::state_committer::FeeMultiplierRange::new_unchecked(
                0.80, 1.20,
            ),
            always_acceptable_fee: 0,
        },
    };

    let fragments = setup.insert_fragments(0, 6).await;

    // Expect a state submission due to nearing max blocks behind and increased tolerance
    let tx_hash = [0; 32];
    let mut l1_mock_submit = test_helpers::mocks::l1::expects_state_submissions([(
        Some(NonEmpty::from_vec(fragments.clone()).unwrap()),
        L1Tx {
            hash: tx_hash,
            nonce: 0,
            ..Default::default()
        },
    )]);
    l1_mock_submit
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(100) }));

    let fuel_mock = test_helpers::mocks::fuel::latest_height_is(80);

    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 6.try_into().unwrap(),
            fee_algo,
            ..Default::default()
        },
        setup.test_clock(),
        preconfigured_fees(fee_sequence),
    );

    // when
    state_committer.run().await?;

    // then
    // Mocks validate that the fragments have been sent due to increased tolerance from nearing max blocks behind
    Ok(())
}

#[tokio::test]
async fn updates_current_height_to_commit_metric_with_latest_bundled_height() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    setup.commit_block_bundle([0; 32], 0, 100).await;

    let l1_mock_submit = mocks::l1::expects_state_submissions(vec![]);

    let fuel_mock = mocks::fuel::latest_height_is(150);

    let registry = prometheus::Registry::new();

    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 10.try_into().unwrap(),
            ..Default::default()
        },
        test_clock.clone(),
        noop_fees(),
    );

    state_committer.register_metrics(&registry);

    // when
    state_committer.run().await?;

    // then
    let gathered_metrics = registry.gather();
    let metric = gathered_metrics
        .iter()
        .find(|m| m.get_name() == "current_height_to_commit")
        .expect("Metric `current_height_to_commit` should be present");

    // Extract the gauge value
    let metric_value = metric
        .get_metric()
        .iter()
        .next()
        .and_then(|m| m.get_gauge().get_value().into())
        .expect("Metric `current_height_to_commit` should have a value");

    assert_eq!(
        metric_value, 101.0,
        "current_height_to_commit should be set to latest_bundled_height + 1 (100 + 1 = 101)"
    );

    Ok(())
}

#[tokio::test]
async fn updates_current_height_to_commit_metric_without_latest_bundled_height() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    // Do NOT commit any block, leaving `latest_bundled_height` as None

    let l1_mock_submit = mocks::l1::expects_state_submissions(vec![]);

    let fuel_mock = mocks::fuel::latest_height_is(150);

    let registry = prometheus::Registry::new();

    let mut state_committer = StateCommitter::new(
        l1_mock_submit,
        fuel_mock,
        setup.db(),
        StateCommitterConfig {
            lookback_window: 100,
            fragment_accumulation_timeout: Duration::from_secs(60),
            fragments_to_accumulate: 10.try_into().unwrap(),
            ..Default::default()
        },
        test_clock.clone(),
        noop_fees(),
    );

    state_committer.register_metrics(&registry);

    // when
    state_committer.run().await?;

    // then
    let gathered_metrics = registry.gather();
    let metric = gathered_metrics
        .iter()
        .find(|m| m.get_name() == "current_height_to_commit")
        .expect("Metric `current_height_to_commit` should be present");

    // Extract the gauge value
    let metric_value = metric
        .get_metric()
        .iter()
        .next()
        .and_then(|m| m.get_gauge().get_value().into())
        .expect("Metric `current_height_to_commit` should have a value");

    assert_eq!(
        metric_value, 50.,
        "current_height_to_commit should be set to latest_height - lookback_window (150 - 100 = 0)"
    );

    Ok(())
}
