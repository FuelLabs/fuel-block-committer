use std::time::Duration;

use eigenda::EigenDAClient;
use metrics::prometheus::IntGauge;
use mockall::predicate::eq;
use services::{
    state_listener::{port::Storage, service::StateListener},
    types::{L1Height, L1Tx, TransactionResponse},
    Result, Runner, StateCommitter, StateCommitterConfig,
};
use test_case::test_case;
use test_helpers::{
    mocks::{self, l1::TxStatus},
    noop_fees,
};

#[tokio::test]
async fn successful_finalized_tx() -> Result<()> {
    use services::state_committer::port::Storage;

    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    let _ = setup.insert_fragments(0, 1).await;

    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;

    let num_blocks_to_finalize = 1u64;
    let current_height = 1;

    let tx_height = current_height - num_blocks_to_finalize;
    let l1_mock = mocks::l1::txs_finished(
        current_height as u32,
        tx_height as u32,
        [(tx_hash, TxStatus::Success)],
    );

    let now = test_clock.now();
    let mut listener = StateListener::new(
        l1_mock,
        setup.db(),
        num_blocks_to_finalize,
        test_clock,
        IntGauge::new("test", "test").unwrap(),
    );

    // when
    listener.run().await.unwrap();

    // then
    assert!(!setup.db().has_pending_txs().await?);
    assert_eq!(setup.db().get_non_finalized_txs().await?.len(), 0);
    assert_eq!(
        setup
            .db()
            .last_time_a_fragment_was_finalized()
            .await?
            .unwrap(),
        now
    );

    Ok(())
}

#[tokio::test]
async fn successful_tx_in_block_not_finalized() -> Result<()> {
    use services::state_committer::port::Storage;

    // given
    let setup = test_helpers::Setup::init().await;

    let _ = setup.insert_fragments(0, 1).await;

    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;

    let num_blocks_to_finalize = 5u64;
    let current_height = 5;

    let tx_height = current_height - 2;
    assert!(current_height - tx_height < num_blocks_to_finalize);

    let l1_mock = mocks::l1::txs_finished(
        current_height as u32,
        tx_height as u32,
        [(tx_hash, TxStatus::Success)],
    );

    let mut listener = StateListener::new(
        l1_mock,
        setup.db(),
        num_blocks_to_finalize,
        setup.test_clock(),
        IntGauge::new("test", "test").unwrap(),
    );

    // when
    listener.run().await.unwrap();

    // then
    assert!(!setup.db().has_pending_txs().await?);
    assert_eq!(setup.db().get_non_finalized_txs().await?.len(), 1);
    assert!(setup
        .db()
        .last_time_a_fragment_was_finalized()
        .await?
        .is_none());

    Ok(())
}

#[tokio::test]
async fn from_pending_to_included_to_success_finalized_tx() -> Result<()> {
    use services::state_committer::port::Storage;

    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    let _ = setup.insert_fragments(0, 1).await;

    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;
    assert!(setup.db().has_pending_txs().await?);

    let num_blocks_to_finalize = 5u64;
    let first_height = 5;
    let second_height = 8;

    let tx_height = first_height - 2;
    assert!(first_height - tx_height < num_blocks_to_finalize);

    let l1_mock = mocks::l1::txs_finished_multiple_heights(
        &[first_height as u32, second_height as u32],
        tx_height as u32,
        [(tx_hash, TxStatus::Success)],
    );

    let mut listener = StateListener::new(
        l1_mock,
        setup.db(),
        num_blocks_to_finalize,
        test_clock.clone(),
        IntGauge::new("test", "test").unwrap(),
    );

    {
        // when first run - pending to included
        listener.run().await.unwrap();

        // then
        assert!(!setup.db().has_pending_txs().await?);
        assert_eq!(setup.db().get_non_finalized_txs().await?.len(), 1);
        assert!(setup
            .db()
            .last_time_a_fragment_was_finalized()
            .await?
            .is_none());
    }
    {
        let now = test_clock.now();

        // when second run - included to finalized
        listener.run().await.unwrap();

        // then
        assert!(!setup.db().has_pending_txs().await?);
        assert_eq!(setup.db().get_non_finalized_txs().await?.len(), 0);
        assert_eq!(
            setup
                .db()
                .last_time_a_fragment_was_finalized()
                .await?
                .unwrap(),
            now
        );
    }

    Ok(())
}

#[tokio::test]
async fn reorg_threw_out_tx_from_block_into_pool() -> Result<()> {
    use services::state_committer::port::Storage;

    // given
    let setup = test_helpers::Setup::init().await;

    let _ = setup.insert_fragments(0, 1).await;

    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;
    assert!(setup.db().has_pending_txs().await?);

    let num_blocks_to_finalize = 5u64;
    let first_height = 5;
    let second_height = 8;

    let tx_height = first_height - 2;
    assert!(first_height - tx_height < num_blocks_to_finalize);

    let l1_mock = mocks::l1::txs_reorg(
        &[first_height as u32, second_height as u32],
        tx_height as u32,
        (tx_hash, TxStatus::Success),
    );

    let mut listener = StateListener::new(
        l1_mock,
        setup.db(),
        num_blocks_to_finalize,
        setup.test_clock(),
        IntGauge::new("test", "test").unwrap(),
    );

    {
        // when first run - pending to included
        listener.run().await.unwrap();

        // then
        assert!(!setup.db().has_pending_txs().await?);
        assert_eq!(setup.db().get_non_finalized_txs().await?.len(), 1);
        assert!(setup
            .db()
            .last_time_a_fragment_was_finalized()
            .await?
            .is_none());
    }
    {
        // when second run - included to pending
        listener.run().await.unwrap();

        // then
        assert!(setup.db().has_pending_txs().await?);
        assert!(setup
            .db()
            .last_time_a_fragment_was_finalized()
            .await?
            .is_none());
    }

    Ok(())
}

#[tokio::test]
async fn reorg_threw_out_tx_from_block_into_pool_and_got_squeezed_out() -> Result<()> {
    use services::state_committer::port::Storage;
    // given
    let setup = test_helpers::Setup::init().await;

    let _ = setup.insert_fragments(0, 1).await;

    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;

    let mut mock = services::state_listener::port::l1::MockApi::new();

    mock.expect_get_transaction_response()
        .once()
        .with(eq(tx_hash))
        .return_once(|_| Box::pin(async { Ok(Some(TransactionResponse::new(1, true, 100, 10))) }));
    mock.expect_get_block_number()
        .returning(|| Box::pin(async { Ok(L1Height::from(1u32)) }));

    let mut listener = StateListener::new(
        mock,
        setup.db(),
        5,
        setup.test_clock(),
        IntGauge::new("test", "test").unwrap(),
    );
    listener.run().await?;

    let mut l1 = services::state_listener::port::l1::MockApi::new();
    l1.expect_get_block_number()
        .returning(|| Box::pin(async { Ok(5.into()) }));
    l1.expect_get_transaction_response()
        .once()
        .with(eq(tx_hash))
        .return_once(|_| Box::pin(async { Ok(None) }));
    l1.expect_is_squeezed_out()
        .once()
        .with(eq(tx_hash))
        .return_once(|_| Box::pin(async { Ok(true) }));
    let mut listener = StateListener::new(
        l1,
        setup.db(),
        5,
        setup.test_clock(),
        IntGauge::new("test", "test").unwrap(),
    );

    // when
    listener.run().await?;

    // then
    let db = setup.db();
    assert!(!db.has_nonfinalized_txs().await?);
    assert!(!db.has_pending_txs().await?);

    Ok(())
}

#[tokio::test]
async fn tx_failed() -> Result<()> {
    use services::state_committer::port::Storage;

    // given
    let setup = test_helpers::Setup::init().await;

    let _ = setup.insert_fragments(0, 1).await;

    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;

    let num_blocks_to_finalize = 5u64;
    let current_height = 5;

    let tx_height = current_height - 2;
    assert!(
            current_height - tx_height < num_blocks_to_finalize,
            "we should choose the tx height such that it's not finalized to showcase that we don't wait for finalization for failed txs"
        );

    let l1_mock = mocks::l1::txs_finished(
        current_height as u32,
        tx_height as u32,
        [(tx_hash, TxStatus::Failure)],
    );

    let mut listener = StateListener::new(
        l1_mock,
        setup.db(),
        num_blocks_to_finalize,
        setup.test_clock(),
        IntGauge::new("test", "test").unwrap(),
    );

    // when
    listener.run().await.unwrap();

    // then
    assert!(!setup.db().has_pending_txs().await?);
    assert!(setup
        .db()
        .last_time_a_fragment_was_finalized()
        .await?
        .is_none());

    Ok(())
}

#[tokio::test]
async fn fine_to_have_nothing_to_check() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let mut listener = StateListener::new(
        services::state_listener::port::l1::MockApi::new(),
        setup.db(),
        5,
        setup.test_clock(),
        IntGauge::new("test", "test").unwrap(),
    );

    // when
    let res = listener.run().await;

    // then
    assert!(res.is_ok());
    Ok(())
}

#[tokio::test]
async fn a_pending_tx_got_squeezed_out() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let _ = setup.insert_fragments(0, 1).await;

    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;
    let mut l1 = services::state_listener::port::l1::MockApi::new();
    l1.expect_get_block_number()
        .returning(|| Box::pin(async { Ok(5.into()) }));

    l1.expect_get_transaction_response()
        .with(eq(tx_hash))
        .once()
        .return_once(|_| Box::pin(async { Ok(None) }));

    l1.expect_is_squeezed_out()
        .with(eq(tx_hash))
        .once()
        .return_once(|_| Box::pin(async { Ok(true) }));

    let mut sut = StateListener::new(
        l1,
        setup.db(),
        5,
        setup.test_clock(),
        IntGauge::new("test", "test").unwrap(),
    );

    // when
    sut.run().await?;

    // then
    assert!(!setup.db().has_pending_txs().await?);

    Ok(())
}

#[tokio::test]
async fn block_inclusion_of_replacement_leaves_no_pending_txs() -> Result<()> {
    use services::state_committer::port::Storage;

    // given
    let setup = test_helpers::Setup::init().await;

    // Insert multiple fragments with the same nonce
    let _ = setup.insert_fragments(0, 1).await;

    let test_clock = setup.test_clock();
    let start_time = test_clock.now();
    let nonce = 0;

    let orig_tx_hash = [0; 32];
    let orig_tx = L1Tx {
        hash: orig_tx_hash,
        created_at: Some(start_time),
        nonce,
        ..Default::default()
    };

    let replacement_tx_time = start_time + Duration::from_secs(1);
    let replacement_tx_hash = [1; 32];
    let replacement_tx = L1Tx {
        hash: replacement_tx_hash,
        created_at: Some(replacement_tx_time),
        nonce,
        ..Default::default()
    };
    let mut l1_mock =
        mocks::l1::expects_state_submissions(vec![(None, orig_tx), (None, replacement_tx)]);
    l1_mock
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(0) }));

    let mut committer = StateCommitter::new(
        l1_mock,
        mocks::fuel::latest_height_is(0),
        setup.db(),
        StateCommitterConfig {
            gas_bump_timeout: Duration::ZERO,
            ..Default::default()
        },
        test_clock.clone(),
        noop_fees(),
        None::<EigenDAClient>,
    );

    // Orig tx
    committer.run().await?;

    // Replacement
    test_clock.set_time(replacement_tx_time);
    committer.run().await?;

    assert_eq!(setup.db().get_non_finalized_txs().await.unwrap().len(), 2);

    let current_height = 10u64;
    let mut l1 = services::state_listener::port::l1::MockApi::new();
    l1.expect_get_block_number()
        .returning(move || Box::pin(async move { Ok(current_height.try_into().unwrap()) }));

    l1.expect_get_transaction_response()
        .with(eq(orig_tx_hash))
        .returning(|_| Box::pin(async { Ok(None) }));
    l1.expect_is_squeezed_out()
        .with(eq(orig_tx_hash))
        .returning(|_| Box::pin(async { Ok(true) }));
    l1.expect_get_transaction_response()
        .with(eq(replacement_tx_hash))
        .once()
        .return_once(move |_| {
            Box::pin(async move {
                Ok(Some(TransactionResponse::new(
                    current_height,
                    true,
                    100,
                    10,
                )))
            })
        });

    let mut listener = StateListener::new(
        l1,
        setup.db(),
        10,
        test_clock,
        IntGauge::new("test", "test").unwrap(),
    );

    // when
    listener.run().await?;

    // then
    let db = setup.db();
    assert!(!db.has_pending_txs().await?);
    assert!(db.has_nonfinalized_txs().await?);

    Ok(())
}

#[test_case(true ; "replacement tx succeeds")]
#[test_case(false ; "replacement tx fails")]
#[tokio::test]
async fn finalized_replacement_tx_will_leave_no_pending_tx(
    replacement_tx_succeeded: bool,
) -> Result<()> {
    use services::state_committer::port::Storage;

    // given
    let setup = test_helpers::Setup::init().await;

    // Insert multiple fragments with the same nonce
    let _ = setup.insert_fragments(0, 1).await;
    let test_clock = setup.test_clock();

    let start_time = test_clock.now();
    let nonce = 0;
    let orig_tx_hash = [0; 32];
    let orig_tx = L1Tx {
        hash: orig_tx_hash,
        created_at: Some(start_time),
        ..Default::default()
    };

    let replacement_tx_time = start_time + Duration::from_secs(1);
    let replacement_tx_hash = [1; 32];
    let replacement_tx = L1Tx {
        hash: replacement_tx_hash,
        created_at: Some(replacement_tx_time),
        nonce,
        ..Default::default()
    };

    let mut l1_mock =
        mocks::l1::expects_state_submissions(vec![(None, orig_tx), (None, replacement_tx)]);
    l1_mock
        .expect_current_height()
        .returning(|| Box::pin(async { Ok(0) }));

    let mut committer = StateCommitter::new(
        l1_mock,
        mocks::fuel::latest_height_is(0),
        setup.db(),
        crate::StateCommitterConfig {
            gas_bump_timeout: Duration::ZERO,
            ..Default::default()
        },
        test_clock.clone(),
        noop_fees(),
        None::<EigenDAClient>,
    );

    // Orig tx
    committer.run().await?;

    // Replacement
    test_clock.set_time(replacement_tx_time);
    committer.run().await?;

    assert_eq!(setup.db().get_non_finalized_txs().await.unwrap().len(), 2);

    let blocks_to_finalize = 1u64;
    let current_height = 10u64;
    let mut l1 = services::state_listener::port::l1::MockApi::new();
    l1.expect_get_block_number()
        .returning(move || Box::pin(async move { Ok(current_height.try_into().unwrap()) }));

    l1.expect_get_transaction_response()
        .with(eq(orig_tx_hash))
        .returning(|_| Box::pin(async { Ok(None) }));
    l1.expect_is_squeezed_out()
        .with(eq(orig_tx_hash))
        .returning(|_| Box::pin(async { Ok(true) }));
    l1.expect_get_transaction_response()
        .with(eq(replacement_tx_hash))
        .once()
        .return_once(move |_| {
            Box::pin(async move {
                Ok(Some(TransactionResponse::new(
                    current_height - blocks_to_finalize,
                    replacement_tx_succeeded,
                    100,
                    10,
                )))
            })
        });

    let mut listener = StateListener::new(
        l1,
        setup.db(),
        blocks_to_finalize,
        test_clock,
        IntGauge::new("test", "test").unwrap(),
    );

    // when
    listener.run().await?;

    // then
    let db = setup.db();
    assert!(!db.has_pending_txs().await?);
    assert!(!db.has_nonfinalized_txs().await?);

    Ok(())
}
