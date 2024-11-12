use metrics::prometheus::IntGauge;
use services::{
    ports::{clock::Clock, storage::Storage},
    Result, Runner, StateListener,
};
use test_helpers::mocks::{self, l1::TxStatus};

#[tokio::test]
async fn state_listener_will_update_tx_state_if_finalized() -> Result<()> {
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
async fn state_listener_will_update_tx_from_pending_to_included() -> Result<()> {
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
async fn state_listener_from_pending_to_included_to_finalized_tx() -> Result<()> {
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
async fn state_listener_from_pending_to_included_to_pending() -> Result<()> {
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
async fn state_listener_will_update_tx_state_if_failed() -> Result<()> {
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
