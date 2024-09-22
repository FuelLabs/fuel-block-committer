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

    async fn oldest_nonfinalized_fragments(&self, limit: usize) -> Result<Vec<BundleFragment>> {
        Ok(self._oldest_nonfinalized_fragments(limit).await?)
    }

    async fn available_blocks(&self) -> Result<Option<RangeInclusive<u32>>> {
        self._available_blocks().await.map_err(Into::into)
    }

    async fn insert_blocks(&self, blocks: NonEmptyVec<ports::storage::FuelBlock>) -> Result<()> {
        Ok(self._insert_blocks(blocks).await?)
    }

    async fn insert_bundle_and_fragments(
        &self,
        block_range: RangeInclusive<u32>,
        fragments: NonEmptyVec<NonEmptyVec<u8>>,
    ) -> Result<NonEmptyVec<BundleFragment>> {
        eprintln!("Inserting bundle and fragments: {:?}", block_range);
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
        fragment_ids: NonEmptyVec<NonNegative<i32>>,
    ) -> Result<()> {
        Ok(self._record_pending_tx(tx_hash, fragment_ids).await?)
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
    use itertools::Itertools;
    use ports::non_empty_vec;
    use ports::storage::{Error, Storage};
    use rand::{thread_rng, Rng, SeedableRng};

    // Helper function to create a storage instance for testing
    async fn start_db() -> DbWithProcess {
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
    async fn can_insert_and_find_latest_block_submission() {
        // Given
        let storage = start_db().await;
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
        let storage = start_db().await;

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
        let storage = start_db().await;

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

    async fn ensure_some_fragments_exists_in_the_db(
        storage: impl Storage,
    ) -> NonEmptyVec<NonNegative<i32>> {
        let ids = storage
            .insert_bundle_and_fragments(
                0..=0,
                non_empty_vec!(non_empty_vec![0], non_empty_vec![1]),
            )
            .await
            .unwrap()
            .into_inner()
            .into_iter()
            .map(|fragment| fragment.id)
            .collect_vec();

        ids.try_into().unwrap()
    }

    #[tokio::test]
    async fn can_record_and_get_pending_txs() {
        // Given
        let storage = start_db().await;

        let fragment_ids = ensure_some_fragments_exists_in_the_db(&storage).await;

        let tx_hash = rand::random::<[u8; 32]>();
        storage
            .record_pending_tx(tx_hash, fragment_ids)
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
        let storage = start_db().await;

        let fragment_ids = ensure_some_fragments_exists_in_the_db(&storage).await;
        let tx_hash = rand::random::<[u8; 32]>();
        storage
            .record_pending_tx(tx_hash, fragment_ids)
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
        let storage = start_db().await;

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
        for (inserted_fragment, fragment_data) in inserted_fragments
            .inner()
            .iter()
            .zip(fragments.inner().iter())
        {
            assert_eq!(inserted_fragment.data, fragment_data.clone());
        }
    }

    fn round_to_millis(date: DateTime<Utc>) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(date.timestamp_millis()).unwrap()
    }

    #[tokio::test]
    async fn can_get_last_time_a_fragment_was_finalized() {
        // Given
        let storage = start_db().await;

        let fragment_ids = ensure_some_fragments_exists_in_the_db(&storage).await;
        let tx_hash = rand::random::<[u8; 32]>();
        storage
            .record_pending_tx(tx_hash, fragment_ids)
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

        assert_eq!(
            round_to_millis(last_time),
            round_to_millis(finalization_time)
        );
    }

    async fn insert_sequence_of_unbundled_blocks(
        storage: impl Storage,
        range: RangeInclusive<u32>,
    ) {
        let mut rng = rand::rngs::SmallRng::from_entropy();
        let blocks = range
            .clone()
            .map(|height| {
                let block_hash: [u8; 32] = rng.gen();
                let block_data = non_empty_vec![height as u8];
                ports::storage::FuelBlock {
                    hash: block_hash,
                    height,
                    data: block_data,
                }
            })
            .collect::<Vec<_>>();

        storage
            .insert_blocks(blocks.try_into().expect("shouldn't be empty"))
            .await
            .unwrap();
    }

    async fn insert_sequence_of_bundled_blocks(storage: impl Storage, range: RangeInclusive<u32>) {
        insert_sequence_of_unbundled_blocks(&storage, range.clone()).await;

        storage
            .insert_bundle_and_fragments(range, non_empty_vec![non_empty_vec![1]])
            .await
            .unwrap();
    }

    async fn lowest_unbundled_sequence(
        storage: impl Storage,
        starting_height: u32,
        limit: usize,
    ) -> RangeInclusive<u32> {
        storage
            .lowest_sequence_of_unbundled_blocks(starting_height, limit)
            .await
            .unwrap()
            .unwrap()
            .height_range()
    }

    #[tokio::test]
    async fn can_get_lowest_sequence_of_unbundled_blocks() {
        // Given
        let storage = start_db().await;

        // Insert blocks 1 to 10
        insert_sequence_of_unbundled_blocks(&storage, 1..=10).await;

        // When
        let height_range = lowest_unbundled_sequence(&storage, 0, usize::MAX).await;

        // Then
        assert_eq!(height_range, 1..=10);
    }

    #[tokio::test]
    async fn handles_holes_in_sequences() {
        // Given
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(&storage, 0..=2).await;
        insert_sequence_of_unbundled_blocks(&storage, 4..=6).await;

        // when
        let height_range = lowest_unbundled_sequence(&storage, 0, usize::MAX).await;

        // then
        assert_eq!(height_range, 0..=2);
    }

    #[tokio::test]
    async fn respects_starting_height() {
        // Given
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(&storage, 0..=10).await;

        // when
        let height_range = lowest_unbundled_sequence(&storage, 2, usize::MAX).await;

        // then
        assert_eq!(height_range, 2..=10);
    }

    #[tokio::test]
    async fn respects_limit() {
        // Given
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(&storage, 0..=10).await;

        // when
        let height_range = lowest_unbundled_sequence(&storage, 0, 2).await;

        // then
        assert_eq!(height_range, 0..=1);
    }

    #[tokio::test]
    async fn ignores_bundled_blocks() {
        // Given
        let storage = start_db().await;

        insert_sequence_of_bundled_blocks(&storage, 0..=2).await;
        insert_sequence_of_unbundled_blocks(&storage, 3..=4).await;

        // when
        let height_range = lowest_unbundled_sequence(&storage, 0, usize::MAX).await;

        // then
        assert_eq!(height_range, 3..=4);
    }

    /// This can happen if we change the lookback config a couple of times in a short period of time
    #[tokio::test]
    async fn can_handle_bundled_blocks_appearing_after_unbundled_ones() {
        // Given
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(&storage, 0..=2).await;
        insert_sequence_of_bundled_blocks(&storage, 7..=10).await;
        insert_sequence_of_unbundled_blocks(&storage, 11..=15).await;

        // when
        let height_range = lowest_unbundled_sequence(&storage, 0, usize::MAX).await;

        // then
        assert_eq!(height_range, 0..=2);
    }

    // Important because sqlx panics if the bundle is too big
    #[tokio::test]
    async fn can_insert_big_batches() {
        let storage = start_db().await;

        // u16::MAX because of implementation details
        insert_sequence_of_bundled_blocks(&storage, 0..=u16::MAX as u32 * 2).await;
    }
    //
    // #[tokio::test]
    // async fn something() {
    //     let port = 5432;
    //
    //     let mut config = DbConfig {
    //         host: "localhost".to_string(),
    //         port,
    //         username: "username".to_owned(),
    //         password: "password".to_owned(),
    //         database: "test".to_owned(),
    //         max_connections: 5,
    //         use_ssl: false,
    //     };
    //     let db = Postgres::connect(&config).await.unwrap();
    //
    //     // u16::MAX because of implementation details
    //     insert_sequence_of_bundled_blocks(&db, 5..=500_000).await;
    //     insert_sequence_of_unbundled_blocks(&db, 500_001..=1_000_000).await;
    //     insert_sequence_of_bundled_blocks(&db, 1_000_001..=1_200_000).await;
    // }
}
