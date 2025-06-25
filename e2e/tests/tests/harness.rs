use std::sync::Arc;

use anyhow::Result;
use e2e_helpers::whole_stack::{FuelNodeType, WholeStack};
use tokio::{sync::Mutex, time::sleep_until};

#[tokio::test(flavor = "multi_thread")]
async fn submitted_correct_block_and_was_finalized() -> Result<()> {
    // given
    let show_logs = false;
    let blob_support = true;
    let stack = WholeStack::deploy_default(show_logs, blob_support).await?;

    // when
    stack
        .fuel_node
        .client()
        .produce_blocks(stack.contract_args.blocks_per_interval)
        .await?;

    // then
    stack
        .committer
        .wait_for_committed_block(stack.contract_args.blocks_per_interval.into())
        .await?;
    let committed_at = tokio::time::Instant::now();

    sleep_until(committed_at + stack.contract_args.finalize_duration).await;

    let latest_block = stack.fuel_node.client().latest_block().await?;

    assert!(stack.deployed_contract.finalized(latest_block).await?);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn submitted_state_and_was_finalized() -> Result<()> {
    // given
    let show_logs = false;
    let blob_support = true;
    let stack = WholeStack::deploy_default(show_logs, blob_support).await?;

    let num_iterations = 10;
    let blocks_per_iteration = 100;

    // when
    for _ in 0..num_iterations {
        let FuelNodeType::Local(node) = &stack.fuel_node else {
            panic!("Expected local fuel node");
        };

        node.produce_transactions(100).await?;

        let _ = stack
            .fuel_node
            .client()
            .produce_blocks(blocks_per_iteration)
            .await;
    }

    // then
    while !state_submitting_finished(&stack.db, num_iterations * blocks_per_iteration).await? {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }

    tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

    let bundle_cost = stack
        .committer
        .fetch_costs(0, 10)
        .await?
        .pop()
        .expect("expected some costs to exist for the committer");
    assert!(bundle_cost.cost > 0);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn old_state_will_be_pruned() -> Result<()> {
    use services::state_pruner::port::Storage;

    // given
    let show_logs = false;
    let blob_support = true;
    let stack = WholeStack::deploy_default(show_logs, blob_support).await?;
    let db_clone = stack.db.clone();
    let sizes = Arc::new(Mutex::new(vec![]));

    let sizes_clone = Arc::clone(&sizes);
    let size_tracking_job = tokio::task::spawn(async move {
        loop {
            let new_sizes = db_clone.table_sizes().await.unwrap();
            sizes_clone.lock().await.push(new_sizes);
            tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
        }
    });

    let num_iterations = 10;
    let blocks_per_iteration = 100;

    // when
    for _ in 0..num_iterations {
        let FuelNodeType::Local(node) = &stack.fuel_node else {
            panic!("Expected local fuel node");
        };

        node.produce_transactions(100).await?;

        let _ = stack
            .fuel_node
            .client()
            .produce_blocks(blocks_per_iteration)
            .await;
    }

    // then
    while !state_submitting_finished(&stack.db, num_iterations * blocks_per_iteration).await? {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }

    tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

    size_tracking_job.abort();

    // validate that sizes went down in size at one point
    let sizes = sizes.lock().await.clone();
    let went_down_in_size = sizes.windows(2).any(|pair| {
        let older = &pair[0];
        let newer = &pair[1];

        newer < older
    });

    assert!(went_down_in_size);

    Ok(())
}

async fn state_submitting_finished(
    db: &storage::DbWithProcess,
    end_range: u32,
) -> anyhow::Result<bool> {
    use services::{
        block_bundler::port::Storage, block_importer::port::Storage as ImporterStorage,
        state_committer::port::Storage as CommitterStorage,
        state_listener::port::Storage as ListenerStorage,
    };

    let finished = db
        .lowest_sequence_of_unbundled_blocks(0, 1)
        .await?
        .is_none()
        && db.oldest_nonfinalized_fragments(0, 1).await?.is_empty()
        && !db.has_pending_txs().await?
        && db.missing_blocks(0, end_range).await?.is_empty();

    Ok(finished)
}
