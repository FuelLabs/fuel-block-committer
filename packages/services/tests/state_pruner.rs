use std::time::Duration;

use ports::storage::Storage;
use services::{state_pruner, Runner};

#[tokio::test]
async fn prune_state() -> services::Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let _ = setup.insert_fragments(0, 1).await;

    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash).await;

    let mut pruner = state_pruner::service::StatePruner::new(setup.db(), Duration::from_secs(10));

    // when
    pruner.run().await?;

    // then
    assert!(!setup.db().has_pending_txs().await?);

    Ok(())
}
