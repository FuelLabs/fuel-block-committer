// #![deny(unused_crate_dependencies)]
mod mappings;
#[cfg(feature = "test-helpers")]
mod test_instance;

use std::ops::RangeInclusive;

#[cfg(feature = "test-helpers")]
pub use test_instance::*;

mod error;
mod postgres;
use ports::{
    storage::{BundleFragment, Result, SequentialFuelBlocks, Storage},
    types::{BlockSubmission, DateTime, L1Tx, NonEmptyVec, NonNegative, TransactionState, Utc},
};
pub use postgres::{DbConfig, Postgres};

impl Storage for Postgres {
    async fn insert(&self, submission: BlockSubmission) -> Result<()> {
        Ok(self._insert(submission).await?)
    }

    async fn oldest_nonfinalized_fragment(&self) -> Result<Option<ports::storage::BundleFragment>> {
        Ok(self._oldest_nonfinalized_fragment().await?)
    }

    async fn available_blocks(&self) -> Result<Option<RangeInclusive<u32>>> {
        self._available_blocks().await.map_err(Into::into)
    }

    async fn insert_block(&self, block: ports::storage::FuelBlock) -> Result<()> {
        Ok(self._insert_block(block).await?)
    }

    async fn is_block_available(&self, hash: &[u8; 32]) -> Result<bool> {
        self._is_block_available(hash).await.map_err(Into::into)
    }

    async fn insert_bundle_and_fragments(
        &self,
        block_range: RangeInclusive<u32>,
        fragments: NonEmptyVec<NonEmptyVec<u8>>,
    ) -> Result<NonEmptyVec<BundleFragment>> {
        Ok(self
            ._insert_bundle_and_fragments(block_range, fragments)
            .await?)
    }

    async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(self._last_time_a_fragment_was_finalized().await?)
    }

    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>> {
        Ok(self._submission_w_latest_block().await?)
    }

    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission> {
        Ok(self._set_submission_completed(fuel_block_hash).await?)
    }

    async fn lowest_sequence_of_unbundled_blocks(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Option<SequentialFuelBlocks>> {
        Ok(self
            ._lowest_unbundled_blocks(starting_height, limit)
            .await?)
    }

    async fn record_pending_tx(
        &self,
        tx_hash: [u8; 32],
        fragment_id: NonNegative<i32>,
    ) -> Result<()> {
        Ok(self._record_pending_tx(tx_hash, fragment_id).await?)
    }

    async fn get_pending_txs(&self) -> Result<Vec<L1Tx>> {
        Ok(self._get_pending_txs().await?)
    }

    async fn has_pending_txs(&self) -> Result<bool> {
        Ok(self._has_pending_txs().await?)
    }

    async fn update_tx_state(&self, hash: [u8; 32], state: TransactionState) -> Result<()> {
        Ok(self._update_tx_state(hash, state).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::storage::{Error, Storage};
    use ports::{non_empty_vec, types::*};
    use rand::{thread_rng, Rng};
    use sqlx::Postgres;
    use std::sync::Arc;

    // Helper function to create a storage instance for testing
    async fn get_test_storage() -> DbWithProcess {
        PostgresProcess::shared()
            .await
            .unwrap()
            .create_random_db()
            .await
            .unwrap()
    }

    fn random_non_zero_height() -> u32 {
        let mut rng = thread_rng();
        rng.gen_range(1..u32::MAX)
    }

    fn given_incomplete_submission(fuel_block_height: u32) -> BlockSubmission {
        BlockSubmission {
            block_hash: rand::random(),
            block_height: fuel_block_height,
            completed: false,
            submittal_height: 0.into(),
        }
    }

    #[tokio::test]
    async fn can_insert_and_find_latest_block() {
        // Given
        let storage = get_test_storage().await;
        let latest_height = random_non_zero_height();

        let latest_submission = given_incomplete_submission(latest_height);
        storage.insert(latest_submission.clone()).await.unwrap();

        let older_submission = given_incomplete_submission(latest_height - 1);
        storage.insert(older_submission).await.unwrap();

        // When
        let actual = storage.submission_w_latest_block().await.unwrap().unwrap();

        // Then
        assert_eq!(actual, latest_submission);
    }

    #[tokio::test]
    async fn can_update_completion_status() {
        // Given
        let storage = get_test_storage().await;

        let height = random_non_zero_height();
        let submission = given_incomplete_submission(height);
        let block_hash = submission.block_hash;
        storage.insert(submission).await.unwrap();

        // When
        let submission = storage.set_submission_completed(block_hash).await.unwrap();

        // Then
        assert!(submission.completed);
    }

    #[tokio::test]
    async fn updating_a_missing_submission_causes_an_error() {
        // Given
        let storage = get_test_storage().await;

        let height = random_non_zero_height();
        let submission = given_incomplete_submission(height);
        let block_hash = submission.block_hash;

        // When
        let result = storage.set_submission_completed(block_hash).await;

        // Then
        if let Err(Error::Database(msg)) = result {
            let block_hash_hex = hex::encode(block_hash);
            assert_eq!(
                msg,
                format!(
                    "Cannot set submission to completed! Submission of block: `{}` not found in DB.",
                    block_hash_hex
                )
            );
        } else {
            panic!("Expected storage error");
        }
    }

    #[tokio::test]
    async fn can_insert_and_check_block_availability() {
        // Given
        let storage = get_test_storage().await;

        let block_hash: [u8; 32] = rand::random();
        let block_height = random_non_zero_height();
        let block_data = non_empty_vec![1u8, 2, 3];

        let block = ports::storage::FuelBlock {
            hash: block_hash,
            height: block_height,
            data: block_data.clone(),
        };
        storage.insert_block(block.clone()).await.unwrap();

        // When
        let is_available = storage.is_block_available(&block_hash).await.unwrap();

        // Then
        assert!(is_available);

        // Check that a non-inserted block is not available
        let other_block_hash: [u8; 32] = rand::random();
        let is_available = storage.is_block_available(&other_block_hash).await.unwrap();
        assert!(!is_available);
    }

    #[tokio::test]
    async fn can_record_and_get_pending_txs() {
        // Given
        let storage = get_test_storage().await;

        let fragment_id = 1.try_into().unwrap();
        let tx_hash = rand::random::<[u8; 32]>();
        storage
            .record_pending_tx(tx_hash, fragment_id)
            .await
            .unwrap();

        // When
        let has_pending = storage.has_pending_txs().await.unwrap();
        let pending_txs = storage.get_pending_txs().await.unwrap();

        // Then
        assert!(has_pending);
        assert_eq!(pending_txs.len(), 1);
        assert_eq!(pending_txs[0].hash, tx_hash);
        assert_eq!(pending_txs[0].state, TransactionState::Pending);
    }

    #[tokio::test]
    async fn can_update_tx_state() {
        // Given
        let storage = get_test_storage().await;

        let fragment_id = 1.try_into().unwrap();
        let tx_hash = rand::random::<[u8; 32]>();
        storage
            .record_pending_tx(tx_hash, fragment_id)
            .await
            .unwrap();

        // When
        storage
            .update_tx_state(tx_hash, TransactionState::Finalized(Utc::now()))
            .await
            .unwrap();

        // Then
        let has_pending = storage.has_pending_txs().await.unwrap();
        let pending_txs = storage.get_pending_txs().await.unwrap();

        assert!(!has_pending);
        assert!(pending_txs.is_empty());
    }

    #[tokio::test]
    async fn can_insert_bundle_and_fragments() {
        // Given
        let storage = get_test_storage().await;

        let block_range = 1..=5;
        let fragment_data1 = NonEmptyVec::try_from(vec![1u8, 2, 3]).unwrap();
        let fragment_data2 = NonEmptyVec::try_from(vec![4u8, 5, 6]).unwrap();
        let fragments =
            NonEmptyVec::try_from(vec![fragment_data1.clone(), fragment_data2.clone()]).unwrap();

        // When
        let inserted_fragments = storage
            .insert_bundle_and_fragments(block_range.clone(), fragments.clone())
            .await
            .unwrap();

        // Then
        assert_eq!(inserted_fragments.len().get(), 2);
        for (inserted_fragment, fragment_data) in inserted_fragments.iter().zip(fragments.iter()) {
            assert_eq!(inserted_fragment.data, fragment_data.clone());
        }
    }

    #[tokio::test]
    async fn can_get_last_time_a_fragment_was_finalized() {
        // Given
        let storage = get_test_storage().await;

        let fragment_id = 1.try_into().unwrap();
        let tx_hash = rand::random::<[u8; 32]>();
        storage
            .record_pending_tx(tx_hash, fragment_id)
            .await
            .unwrap();

        let finalization_time = Utc::now();

        // When
        storage
            .update_tx_state(tx_hash, TransactionState::Finalized(finalization_time))
            .await
            .unwrap();

        // Then
        let last_time = storage
            .last_time_a_fragment_was_finalized()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(last_time, finalization_time);
    }

    #[tokio::test]
    async fn can_get_lowest_sequence_of_unbundled_blocks() {
        // Given
        let storage = get_test_storage().await;

        // Insert blocks 1 to 10
        for height in 1..=10 {
            let block_hash: [u8; 32] = rand::random();
            let block_data = non_empty_vec![height as u8];
            let block = ports::storage::FuelBlock {
                hash: block_hash,
                height,
                data: block_data,
            };
            storage.insert_block(block).await.unwrap();
        }

        // When
        let starting_height = 1;
        let limit = 5;
        let sequence = storage
            .lowest_sequence_of_unbundled_blocks(starting_height, limit)
            .await
            .unwrap()
            .unwrap();

        // Then
        assert_eq!(sequence.len().get(), 5);
        assert_eq!(sequence.first().height, starting_height);
    }
}
