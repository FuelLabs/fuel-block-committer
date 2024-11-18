use services::{block_committer::service::BlockCommitter, Runner};
use services::{
    ports::storage::Storage,
    types::{TransactionResponse, TransactionState, Utc},
};
use test_helpers::{
    mocks::fuel::{given_a_block, given_fetcher, given_secret_key},
    mocks::l1::{expects_contract_submission, expects_transaction_response, FullL1Mock},
};

#[tokio::test]
async fn will_do_nothing_if_latest_block_is_completed_and_not_stale() {
    // given
    let setup = test_helpers::Setup::init().await;

    let secret_key = given_secret_key();
    let latest_block = given_a_block(10, &secret_key);
    let fuel_adapter = given_fetcher(vec![latest_block.clone()]);

    setup.add_submissions(vec![6, 8, 10]).await;
    setup // TODO: do this through block committer and not directly
        .db()
        .update_block_submission_tx([10; 32], TransactionState::Finalized(Utc::now()))
        .await
        .unwrap();

    let mut l1 = FullL1Mock::new();
    l1.contract.expect_submit().never();

    let mut block_committer = BlockCommitter::new(
        l1,
        setup.db(),
        fuel_adapter,
        setup.test_clock(),
        2.try_into().unwrap(),
        1,
    );

    // when
    block_committer.run().await.unwrap();

    // MockL1 verifies that submit was not called
}

#[tokio::test]
async fn will_submit_on_latest_epoch() {
    // given
    let setup = test_helpers::Setup::init().await;

    let latest_block = given_a_block(10, &given_secret_key());
    let fuel_adapter = given_fetcher(vec![latest_block.clone()]);

    let l1 = expects_contract_submission(latest_block, [0; 32]);
    let mut block_committer = BlockCommitter::new(
        l1,
        setup.db(),
        fuel_adapter,
        setup.test_clock(),
        2.try_into().unwrap(),
        1,
    );

    // when
    block_committer.run().await.unwrap();

    // MockL1 validates the expected calls are made
}

#[tokio::test]
async fn will_skip_incomplete_submission_to_submit_latest() {
    // given
    let setup = test_helpers::Setup::init().await;

    let secret_key = given_secret_key();
    let latest_block = given_a_block(10, &secret_key);
    let all_blocks = vec![
        given_a_block(8, &secret_key),
        given_a_block(9, &secret_key),
        latest_block.clone(),
    ];
    let fuel_adapter = given_fetcher(all_blocks);

    let l1 = expects_contract_submission(latest_block, [0; 32]);
    setup.add_submissions(vec![8]).await;

    let mut block_committer = BlockCommitter::new(
        l1,
        setup.db(),
        fuel_adapter,
        setup.test_clock(),
        2.try_into().unwrap(),
        1,
    );

    // when
    block_committer.run().await.unwrap();

    // MockL1 validates the expected calls are made
}

#[tokio::test]
async fn will_fetch_and_submit_missed_block() {
    // given
    let setup = test_helpers::Setup::init().await;

    let secret_key = given_secret_key();
    let missed_block = given_a_block(4, &secret_key);
    let latest_block = given_a_block(5, &secret_key);
    let fuel_adapter = given_fetcher(vec![latest_block, missed_block.clone()]);

    let l1 = expects_contract_submission(missed_block, [3; 32]);
    setup.add_submissions(vec![0, 2]).await;

    let mut block_committer = BlockCommitter::new(
        l1,
        setup.db(),
        fuel_adapter,
        setup.test_clock(),
        2.try_into().unwrap(),
        1,
    );

    // when
    block_committer.run().await.unwrap();

    // then
    // MockL1 validates the expected calls are made
}

#[tokio::test]
async fn will_not_reattempt_submitting_missed_block() {
    // given
    let setup = test_helpers::Setup::init().await;

    let secret_key = given_secret_key();
    let missed_block = given_a_block(4, &secret_key);
    let latest_block = given_a_block(5, &secret_key);
    let fuel_adapter = given_fetcher(vec![latest_block, missed_block]);

    setup.add_submissions(vec![0, 2, 4]).await;

    let l1 = expects_transaction_response(5, [4; 32], None);

    let mut block_committer = BlockCommitter::new(
        l1,
        setup.db(),
        fuel_adapter,
        setup.test_clock(),
        2.try_into().unwrap(),
        1,
    );

    // when
    block_committer.run().await.unwrap();

    // then
    // Mock verifies that the submit didn't happen
}

#[tokio::test]
async fn propagates_block_if_epoch_reached() {
    // given
    let setup = test_helpers::Setup::init().await;

    let secret_key = given_secret_key();
    let block = given_a_block(4, &secret_key);
    let fuel_adapter = given_fetcher(vec![block.clone()]);

    setup.add_submissions(vec![0, 2]).await;
    let l1 = expects_contract_submission(block, [1; 32]);
    let mut block_committer = BlockCommitter::new(
        l1,
        setup.db(),
        fuel_adapter,
        setup.test_clock(),
        2.try_into().unwrap(),
        1,
    );

    // when
    block_committer.run().await.unwrap();

    // then
    // Mock verifies that submit was called with the appropriate block
}

#[tokio::test]
async fn updates_submission_state_to_finalized() {
    // given
    let setup = test_helpers::Setup::init().await;

    let secret_key = given_secret_key();
    let latest_height = 4;
    let latest_block = given_a_block(latest_height, &secret_key);
    let fuel_adapter = given_fetcher(vec![latest_block]);

    setup.add_submissions(vec![0, 2, 4]).await;
    let tx_response = TransactionResponse::new(latest_height as u64, true);
    let l1 = expects_transaction_response(latest_height, [4; 32], Some(tx_response));

    let mut block_committer = BlockCommitter::new(
        l1,
        setup.db().clone(),
        fuel_adapter,
        setup.test_clock(),
        2.try_into().unwrap(),
        1,
    );

    // when
    block_committer.run().await.unwrap();

    // then
    let latest_submission = setup
        .db()
        .submission_w_latest_block()
        .await
        .unwrap()
        .expect("submission to exist");
    let pending_txs = setup
        .db()
        .get_pending_block_submission_txs(latest_submission.id.expect("submission to have id"))
        .await
        .unwrap();

    assert!(pending_txs.is_empty());
}
