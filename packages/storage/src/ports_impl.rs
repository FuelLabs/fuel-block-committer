use crate::Postgres;
use services::{state_pruner, Result};

impl state_pruner::port::Storage for Postgres {
    async fn prune_entries_older_than(
        &self,
        date: state_pruner::port::DateTime<state_pruner::port::Utc>,
    ) -> Result<state_pruner::port::Pruned> {
        let response = sqlx::query!(
            r#"
            WITH

            -- Delete from l1_blob_transaction
            deleted_blob_transactions AS (
                DELETE FROM l1_blob_transaction
                WHERE created_at < $1
                RETURNING id
            ),

            -- Delete from l1_transaction_fragments
            deleted_transaction_fragments AS (
                DELETE FROM l1_transaction_fragments
                WHERE transaction_id IN (SELECT id FROM deleted_blob_transactions)
                RETURNING transaction_id
            ),

            -- Build updated_transaction_fragments that represent the state after deletions
            updated_transaction_fragments AS (
                SELECT fragment_id FROM l1_transaction_fragments
                WHERE transaction_id NOT IN (SELECT transaction_id FROM deleted_transaction_fragments)
            ),

            -- Delete unreferenced fragments
            deleted_fragments AS (
                DELETE FROM l1_fragments
                WHERE id NOT IN (SELECT fragment_id FROM updated_transaction_fragments)
                RETURNING id
            ),

            -- Step 4: Build updated_fragments that represent the state of after deletions
            updated_fragments AS (
                SELECT bundle_id FROM l1_fragments
                WHERE id NOT IN (SELECT id FROM deleted_fragments)
            ),

            -- Delete unreferenced bundles and collect start and end heights
            deleted_bundles AS (
                DELETE FROM bundles
                WHERE id NOT IN (SELECT bundle_id FROM updated_fragments)
                RETURNING start_height, end_height
            ),

            -- Delete corresponding fuel_blocks entries
            deleted_fuel_blocks AS (
                DELETE FROM fuel_blocks
                WHERE EXISTS (
                    SELECT 1 FROM deleted_bundles
                    WHERE fuel_blocks.height BETWEEN deleted_bundles.start_height AND deleted_bundles.end_height
                )
                RETURNING height
            ),

            -- Delete from l1_transaction
            deleted_transactions AS (
                DELETE FROM l1_transaction
                WHERE created_at < $1
                RETURNING id
            ),

            -- Build updated_contract transaction that represent the state after deletions
            updated_transactions AS (
                SELECT submission_id FROM l1_transaction
                WHERE id NOT IN (SELECT id FROM deleted_transactions)
            ),

            -- Delete from l1_fuel_block_submission
            deleted_submissions AS (
                DELETE FROM l1_fuel_block_submission
                WHERE id NOT IN (SELECT submission_id FROM updated_transactions)
                RETURNING id
            )

            SELECT
                (SELECT COUNT(*) FROM deleted_blob_transactions) AS deleted_blob_transactions,
                (SELECT COUNT(*) FROM deleted_transaction_fragments) AS deleted_transaction_fragments,
                (SELECT COUNT(*) FROM deleted_fragments) AS deleted_fragments,
                (SELECT COUNT(*) FROM deleted_bundles) AS deleted_bundles,
                (SELECT COUNT(*) FROM deleted_fuel_blocks) AS deleted_fuel_blocks,
                (SELECT COUNT(*) FROM deleted_transactions) AS deleted_contract_transactions,
                (SELECT COUNT(*) FROM deleted_submissions) AS deleted_contract_submissions;
            "#,
           date
        )
        .fetch_one(&self.connection_pool)
        .await.map_err(|e| services::Error::Storage(e.to_string()))?;

        Ok(state_pruner::port::Pruned {
            blob_transactions: response.deleted_blob_transactions.unwrap_or_default() as u32,
            fragments: response.deleted_fragments.unwrap_or_default() as u32,
            transaction_fragments: response.deleted_transaction_fragments.unwrap_or_default()
                as u32,
            bundles: response.deleted_bundles.unwrap_or_default() as u32,
            blocks: response.deleted_fuel_blocks.unwrap_or_default() as u32,
            contract_transactions: response.deleted_contract_transactions.unwrap_or_default()
                as u32,
            contract_submisions: response.deleted_contract_submissions.unwrap_or_default() as u32,
        })
    }

    async fn table_sizes(&self) -> Result<state_pruner::port::TableSizes> {
        let response = sqlx::query!(
            r#"
            SELECT
                (SELECT COUNT(*) FROM l1_blob_transaction) AS size_blob_transactions,
                (SELECT COUNT(*) FROM l1_transaction_fragments) AS size_transaction_fragments,
                (SELECT COUNT(*) FROM l1_fragments) AS size_fragments,
                (SELECT COUNT(*) FROM bundles) AS size_bundles,
                (SELECT COUNT(*) FROM fuel_blocks) AS size_fuel_blocks,
                (SELECT COUNT(*) FROM l1_transaction) AS size_contract_transactions,
                (SELECT COUNT(*) FROM l1_fuel_block_submission) AS size_contract_submisions
            "#,
        )
        .fetch_one(&self.connection_pool)
        .await
        .map_err(|e| services::Error::Storage(e.to_string()))?;

        Ok(state_pruner::port::TableSizes {
            blob_transactions: response.size_blob_transactions.unwrap_or_default() as u32,
            fragments: response.size_fragments.unwrap_or_default() as u32,
            transaction_fragments: response.size_transaction_fragments.unwrap_or_default() as u32,
            bundles: response.size_bundles.unwrap_or_default() as u32,
            blocks: response.size_fuel_blocks.unwrap_or_default() as u32,
            contract_transactions: response.size_contract_transactions.unwrap_or_default() as u32,
            contract_submisions: response.size_contract_submisions.unwrap_or_default() as u32,
        })
    }
}
