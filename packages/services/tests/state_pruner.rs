use std::time::Duration;

use ports::storage::Storage;
use services::{state_pruner, state_pruner::port::Storage as PrunerStorage, Runner};

#[tokio::test]
async fn prune_state() -> services::Result<()> {
    // given
    let setup = test_helpers::Setup::init().await; // TODO: @hal3e use one test clock
    let test_clock = setup.test_clock();

    let fragments = setup.insert_fragments(0, 10).await;
    let num_fagments = fragments.len();
    dbg!(num_fagments);

    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash).await;

    test_clock.advance_time(Duration::from_secs(2));

    let table_sizes = setup.db().table_sizes().await?;
    dbg!(&table_sizes);

    tokio::time::sleep(Duration::from_secs(4)).await;

    let mut pruner = state_pruner::service::StatePruner::new(setup.db(), Duration::from_secs(2));

    // when
    pruner.run().await?;

    let table_sizes = setup.db().table_sizes().await?;
    dbg!(&table_sizes);

    // then
    assert!(!setup.db().has_pending_txs().await?);

    Ok(())
}
