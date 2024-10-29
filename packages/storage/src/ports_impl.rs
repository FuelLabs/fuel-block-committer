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

            -- Step 1: Identify old transactions
            old_transactions AS (
                SELECT id FROM l1_blob_transaction
                WHERE created_at < $1
            ),

            -- Step 2: Delete from l1_transaction_fragments and collect fragment_ids
            deleted_transaction_fragments AS (
                DELETE FROM l1_transaction_fragments
                WHERE transaction_id IN (SELECT id FROM old_transactions)
                RETURNING fragment_id
            ),

            -- Step 3: Delete from l1_blob_transaction
            deleted_blob_transactions AS (
                DELETE FROM l1_blob_transaction
                WHERE id IN (SELECT id FROM old_transactions)
                RETURNING id
            ),

            -- Step 4: Delete unreferenced fragments and collect bundle IDs
            deleted_fragments AS (
                DELETE FROM l1_fragments
                WHERE id NOT IN (SELECT DISTINCT fragment_id FROM l1_transaction_fragments)
                RETURNING bundle_id
            ),

            -- Step 5: Delete unreferenced bundles and collect start and end heights
            deleted_bundles AS (
                DELETE FROM bundles
                WHERE id NOT IN (SELECT DISTINCT bundle_id FROM l1_fragments)
                RETURNING start_height, end_height
            ),

            -- Step 6: Delete corresponding fuel_blocks entries
            deleted_fuel_blocks AS (
                DELETE FROM fuel_blocks
                WHERE EXISTS (
                    SELECT 1 FROM deleted_bundles
                    WHERE fuel_blocks.height BETWEEN deleted_bundles.start_height AND deleted_bundles.end_height
                )
                RETURNING height
            )

            SELECT
                (SELECT COUNT(*) FROM deleted_blob_transactions) AS deleted_blob_transactions,
                (SELECT COUNT(*) FROM deleted_transaction_fragments) AS deleted_transaction_fragments,
                (SELECT COUNT(*) FROM deleted_fragments) AS deleted_fragments,
                (SELECT COUNT(*) FROM deleted_bundles) AS deleted_bundles,
                (SELECT COUNT(*) FROM deleted_fuel_blocks) AS deleted_fuel_blocks;
            "#,
           date
        )
        .fetch_one(&self.connection_pool)
        .await.map_err(|e| services::Error::Storage(e.to_string()))?;

        Ok(state_pruner::port::Pruned {
            blob_transactions: response.deleted_blob_transactions.unwrap_or_default() as u32,
            fragments: response.deleted_fragments.unwrap_or_default() as u32,
            bundles: response.deleted_bundles.unwrap_or_default() as u32,
            blocks: response.deleted_fuel_blocks.unwrap_or_default() as u32,
            contract_transactions: 0,
            contract_submisions: 0,
        })
    }
}
