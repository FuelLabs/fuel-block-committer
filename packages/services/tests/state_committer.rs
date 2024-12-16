use services::{
    fee_analytics::{
        port::{l1::testing::ConstantFeesProvider, Fees},
        service::FeeAnalytics,
    },
    types::{L1Tx, NonEmpty},
    Result, Runner, StateCommitter, StateCommitterConfig,
};
use std::time::Duration;

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
        FeeAnalytics::new(ConstantFeesProvider::new(Fees::default())),
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
        FeeAnalytics::new(ConstantFeesProvider::new(Fees::default())),
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
        FeeAnalytics::new(ConstantFeesProvider::new(Fees::default())),
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
        FeeAnalytics::new(ConstantFeesProvider::new(Fees::default())),
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
        FeeAnalytics::new(ConstantFeesProvider::new(Fees::default())),
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
        FeeAnalytics::new(ConstantFeesProvider::new(Fees::default())),
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

    l1_mock_submit.expect_current_height().returning(|| {
        eprintln!("I was called");
        Box::pin(async { Ok(0) })
    });

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
        FeeAnalytics::new(ConstantFeesProvider::new(Fees::default())),
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
