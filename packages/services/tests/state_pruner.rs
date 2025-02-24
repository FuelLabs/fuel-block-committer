use std::time::Duration;

use services::{Runner, state_pruner, state_pruner::port::Storage as PrunerStorage};

#[tokio::test]
async fn not_pruning_until_retention_exceeded() -> services::Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    setup.submit_contract_transaction(10, [1; 32]).await;
    setup.commit_block_bundle([1; 32], 0, 0).await;
    test_clock.advance_time(Duration::from_secs(1));

    setup.commit_block_bundle([2; 32], 1, 1).await;
    test_clock.advance_time(Duration::from_secs(1));

    setup.commit_block_bundle([3; 32], 2, 2).await;
    setup.submit_contract_transaction(20, [4; 32]).await;

    let pre_pruning_table_sizes = setup.db().table_sizes().await?;
    assert_eq!(pre_pruning_table_sizes.blob_transactions, 3);
    assert_eq!(pre_pruning_table_sizes.fragments, 18);
    assert_eq!(pre_pruning_table_sizes.transaction_fragments, 18);
    assert_eq!(pre_pruning_table_sizes.bundles, 3);
    assert_eq!(pre_pruning_table_sizes.blocks, 3);
    assert_eq!(pre_pruning_table_sizes.contract_transactions, 2);
    assert_eq!(pre_pruning_table_sizes.contract_submissions, 2);

    let mut pruner = state_pruner::service::StatePruner::new(
        setup.db(),
        test_clock.clone(),
        Duration::from_secs(10),
    );

    // when
    pruner.run().await?;

    // then
    let post_pruning_table_sizes = setup.db().table_sizes().await?;
    assert_eq!(pre_pruning_table_sizes, post_pruning_table_sizes);

    Ok(())
}

#[tokio::test]
async fn prune_old_transactions() -> services::Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    setup.submit_contract_transaction(10, [0; 32]).await;
    setup.commit_block_bundle([1; 32], 0, 0).await;
    test_clock.advance_time(Duration::from_secs(10));

    setup.commit_block_bundle([2; 32], 1, 1).await;
    test_clock.advance_time(Duration::from_secs(10));

    setup.commit_block_bundle([3; 32], 2, 2).await;
    setup.submit_contract_transaction(20, [4; 32]).await;

    let table_sizes = setup.db().table_sizes().await?;
    assert_eq!(table_sizes.blob_transactions, 3);
    assert_eq!(table_sizes.fragments, 18);
    assert_eq!(table_sizes.transaction_fragments, 18);
    assert_eq!(table_sizes.bundles, 3);
    assert_eq!(table_sizes.blocks, 3);
    assert_eq!(table_sizes.contract_transactions, 2);
    assert_eq!(table_sizes.contract_submissions, 2);

    let mut pruner = state_pruner::service::StatePruner::new(
        setup.db(),
        test_clock.clone(),
        Duration::from_secs(2),
    );

    // when
    pruner.run().await?;

    // then
    let table_sizes = setup.db().table_sizes().await?;
    assert_eq!(table_sizes.blob_transactions, 1);
    assert_eq!(table_sizes.fragments, 6);
    assert_eq!(table_sizes.transaction_fragments, 6);
    assert_eq!(table_sizes.bundles, 1);
    assert_eq!(table_sizes.blocks, 1);
    assert_eq!(table_sizes.contract_transactions, 1);
    assert_eq!(table_sizes.contract_submissions, 1);

    Ok(())
}

#[tokio::test]
async fn will_not_prune_fragments_if_referenced_in_a_tx() -> services::Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    let _ = setup.insert_fragments(0, 3).await;

    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;
    test_clock.advance_time(Duration::from_secs(400));

    // create replacement tx that will reference the fragments again
    let tx_hash = [1; 32];
    setup.send_fragments(tx_hash, 0).await;

    let table_sizes = setup.db().table_sizes().await?;
    assert_eq!(table_sizes.blob_transactions, 2);
    assert_eq!(table_sizes.fragments, 3);
    assert_eq!(table_sizes.transaction_fragments, 6);
    assert_eq!(table_sizes.bundles, 1);
    assert_eq!(table_sizes.blocks, 1);

    let mut pruner = state_pruner::service::StatePruner::new(
        setup.db(),
        test_clock.clone(),
        Duration::from_secs(2),
    );

    // when
    pruner.run().await?;

    // then
    let table_sizes = setup.db().table_sizes().await?;
    assert_eq!(table_sizes.blob_transactions, 1);
    assert_eq!(table_sizes.fragments, 3);
    assert_eq!(table_sizes.transaction_fragments, 3);
    assert_eq!(table_sizes.bundles, 1);
    assert_eq!(table_sizes.blocks, 1);

    Ok(())
}
