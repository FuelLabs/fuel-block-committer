use ports::{storage::Storage, types::BundleCost};

use crate::Result;

pub struct CostReporter<Db> {
    storage: Db,
}

impl<Db> CostReporter<Db> {
    pub fn new(storage: Db) -> Self {
        Self { storage }
    }
}

impl<Db> CostReporter<Db>
where
    Db: Storage,
{
    pub async fn get_costs(&self, from_block_height: u32, limit: usize) -> Result<Vec<BundleCost>> {
        Ok(self
            .storage
            .get_finalized_costs(from_block_height, limit)
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::types::{nonempty, Fragment, L1Tx, NonEmpty, TransactionState, Utc};
    use storage::PostgresProcess;

    // Helper function to create a storage instance for testing
    async fn start_db() -> impl Storage {
        PostgresProcess::shared()
            .await
            .unwrap()
            .create_random_db()
            .await
            .unwrap()
    }

    // Helper function to insert a finalized bundle into the database
    async fn insert_finalized_bundle(
        storage: &impl Storage,
        start_height: u32,
        end_height: u32,
        total_fee: u128,
        size: u32,
        da_block_height: u64,
    ) -> Result<()> {
        // Insert a bundle with fragments
        let fragment = Fragment {
            data: nonempty![rand::random::<u8>()],
            unused_bytes: 0,
            total_bytes: size.try_into().unwrap(),
        };
        let fragments = nonempty![fragment.clone()];

        storage
            .insert_bundle_and_fragments(start_height..=end_height, fragments.clone())
            .await
            .unwrap();

        // Get fragments from the database
        let fragment_in_db = storage
            .oldest_nonfinalized_fragments(0, 1)
            .await
            .unwrap()
            .pop()
            .unwrap();

        // Create a transaction and associate it with the fragment
        let tx_hash = rand::random::<[u8; 32]>();
        let tx = L1Tx {
            hash: tx_hash,
            nonce: rand::random(),
            ..Default::default()
        };
        storage
            .record_pending_tx(
                tx.clone(),
                NonEmpty::from_vec(vec![fragment_in_db.id]).unwrap(),
            )
            .await
            .unwrap();

        // Finalize the transaction
        let finalization_time = Utc::now();
        let changes = vec![(
            tx.hash,
            tx.nonce,
            TransactionState::Finalized(finalization_time),
        )];
        storage.batch_update_tx_states(vec![], changes).await?;

        // Update costs
        let cost_per_tx = vec![(tx_hash, total_fee, da_block_height)];
        storage.update_costs(cost_per_tx).await?;

        Ok(())
    }

    #[tokio::test]
    async fn get_costs_returns_empty_when_no_bundles() {
        // given
        let storage = start_db().await;
        let cost_reporter = CostReporter::new(storage);

        // when
        let costs = cost_reporter.get_costs(0, 10).await.unwrap();

        // then
        assert!(costs.is_empty());
    }

    #[tokio::test]
    async fn get_costs_returns_only_finalized_bundles() {
        // given
        let storage = start_db().await;
        let cost_reporter = CostReporter::new(&storage);

        // Insert a finalized bundle
        insert_finalized_bundle(&storage, 1, 5, 1000u128, 500u32, 10000u64)
            .await
            .unwrap();

        // Insert a non-finalized bundle
        let fragment = Fragment {
            data: nonempty![rand::random::<u8>()],
            unused_bytes: 0,
            total_bytes: 600.try_into().unwrap(),
        };
        let fragments = nonempty![fragment.clone()];

        storage
            .insert_bundle_and_fragments(6..=10, fragments.clone())
            .await
            .unwrap();

        // when
        let costs = cost_reporter.get_costs(0, 10).await.unwrap();

        // then
        assert_eq!(costs.len(), 1);

        let bundle_cost = &costs[0];
        assert_eq!(bundle_cost.start_height, 1);
        assert_eq!(bundle_cost.end_height, 5);
    }

    #[tokio::test]
    async fn get_costs_respects_from_block_height_and_limit() {
        // given
        let storage = start_db().await;
        let cost_reporter = CostReporter::new(&storage);

        // Insert multiple finalized bundles
        for i in 0..5 {
            let start_height = i * 10 + 1;
            let end_height = start_height + 9;
            let total_fee = 1000u128 + (i as u128) * 500u128;
            let size = 500u32 + (i as u32) * 100u32;
            let da_block_height = 10000u64 + (i as u64);

            insert_finalized_bundle(
                &storage,
                start_height,
                end_height,
                total_fee,
                size,
                da_block_height,
            )
            .await
            .unwrap();
        }

        // when
        let from_block_height = 21;
        let limit = 2;
        let costs = cost_reporter
            .get_costs(from_block_height, limit)
            .await
            .unwrap();

        // then
        assert_eq!(costs.len(), 2);

        // Verify that the bundles have starting heights >= from_block_height
        assert!(costs[0].start_height >= from_block_height as u64);
        assert!(costs[1].start_height >= from_block_height as u64);

        // Verify that the limit is respected
        assert_eq!(costs.len(), limit);
    }

    #[tokio::test]
    async fn get_costs_handles_no_finalized_bundles_after_from_block_height() {
        // given
        let storage = start_db().await;
        let cost_reporter = CostReporter::new(&storage);

        // Insert finalized bundles below the from_block_height
        insert_finalized_bundle(&storage, 1, 5, 1000u128, 500u32, 10000u64)
            .await
            .unwrap();
        insert_finalized_bundle(&storage, 6, 10, 1500u128, 600u32, 10001u64)
            .await
            .unwrap();

        let from_block_height = 20;

        // when
        let costs = cost_reporter
            .get_costs(from_block_height, 10)
            .await
            .unwrap();

        // then
        assert!(costs.is_empty());
    }
}
