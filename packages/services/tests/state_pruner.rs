use std::time::Duration;

use services::{state_pruner, state_pruner::port::Storage as PrunerStorage, Runner};

#[tokio::test]
async fn not_pruning_until_retention_exceeded() -> services::Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    setup.commit_block_bundle([0; 32], 0, 0).await;
    test_clock.advance_time(Duration::from_secs(1));

    setup.commit_block_bundle([1; 32], 1, 1).await;
    test_clock.advance_time(Duration::from_secs(1));

    setup.commit_block_bundle([2; 32], 2, 2).await;

    let table_sizes = setup.db().table_sizes().await?;
    assert_eq!(table_sizes.blob_transactions, 3);
    assert_eq!(table_sizes.fragments, 18);
    assert_eq!(table_sizes.transaction_fragments, 18);
    assert_eq!(table_sizes.bundles, 3);
    assert_eq!(table_sizes.blocks, 3);

    let mut pruner = state_pruner::service::StatePruner::new(
        setup.db(),
        test_clock.clone(),
        Duration::from_secs(10),
    );

    // when
    pruner.run().await?;

    // then
    let table_sizes = setup.db().table_sizes().await?;
    assert_eq!(table_sizes.blob_transactions, 3);
    assert_eq!(table_sizes.fragments, 18);
    assert_eq!(table_sizes.transaction_fragments, 18);
    assert_eq!(table_sizes.bundles, 3);
    assert_eq!(table_sizes.blocks, 3);

    Ok(())
}

#[tokio::test]
async fn prune_old_transactions() -> services::Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let test_clock = setup.test_clock();

    setup.commit_block_bundle([0; 32], 0, 0).await;
    test_clock.advance_time(Duration::from_secs(10));

    setup.commit_block_bundle([1; 32], 1, 1).await;
    test_clock.advance_time(Duration::from_secs(10));

    setup.commit_block_bundle([2; 32], 2, 2).await;

    let table_sizes = setup.db().table_sizes().await?;
    assert_eq!(table_sizes.blob_transactions, 3);
    assert_eq!(table_sizes.fragments, 18);
    assert_eq!(table_sizes.transaction_fragments, 18);
    assert_eq!(table_sizes.bundles, 3);
    assert_eq!(table_sizes.blocks, 3);

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

    Ok(())
}
