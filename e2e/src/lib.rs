#[cfg(test)]
mod committer;
#[cfg(test)]
mod eth_node;
#[cfg(test)]
mod fuel_node;
#[cfg(test)]
mod kms;
#[cfg(test)]
mod whole_stack;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anyhow::Result;
    use tokio::time::sleep_until;

    use crate::whole_stack::{FuelNodeType, WholeStack};

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
            .wait_for_committed_block(stack.contract_args.blocks_per_interval as u64)
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

        let bundle_cost = stack.committer.fetch_costs(0, 10).await?.pop().unwrap();
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

        let expected_table_sizes_before_pruning = services::state_pruner::port::TableSizes {
            blob_transactions: 2,
            fragments: 2,
            transaction_fragments: 2,
            bundles: 2,
            bundle_costs: 2,
            blocks: 1300,
            contract_transactions: 2,
            contract_submissions: 2,
        };
        let table_sizes = stack.db.table_sizes().await?;
        if table_sizes < expected_table_sizes_before_pruning {
            panic!("Expected {table_sizes:#?} >= {expected_table_sizes_before_pruning:#?}");
        }

        // Sleep to make sure the pruner service had time to run
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        let table_sizes = stack.db.table_sizes().await?;
        assert!(table_sizes < expected_table_sizes_before_pruning);

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

    #[ignore = "meant for running manually and tweaking configuration parameters"]
    #[tokio::test(flavor = "multi_thread")]
    async fn connecting_to_testnet() -> Result<()> {
        // given
        let show_logs = false;
        let blob_support = true;
        let _stack = WholeStack::connect_to_testnet(show_logs, blob_support).await?;

        tokio::time::sleep(Duration::from_secs(10000)).await;

        Ok(())
    }
}
