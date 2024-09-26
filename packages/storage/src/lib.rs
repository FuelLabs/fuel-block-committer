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
    types::{
        BlockSubmission, DateTime, Fragment, L1Tx, NonEmpty, NonNegative, TransactionState, Utc,
    },
};
pub use postgres::{DbConfig, Postgres};

impl Storage for Postgres {
    async fn insert(&self, submission: BlockSubmission) -> Result<()> {
        Ok(self._insert(submission).await?)
    }

    async fn oldest_nonfinalized_fragments(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Vec<BundleFragment>> {
        Ok(self
            ._oldest_nonfinalized_fragments(starting_height, limit)
            .await?)
    }

    async fn missing_blocks(
        &self,
        starting_height: u32,
        current_height: u32,
    ) -> Result<Vec<RangeInclusive<u32>>> {
        self._missing_blocks(starting_height, current_height)
            .await
            .map_err(Into::into)
    }

    async fn insert_blocks(&self, blocks: NonEmpty<ports::storage::FuelBlock>) -> Result<()> {
        Ok(self._insert_blocks(blocks).await?)
    }

    async fn insert_bundle_and_fragments(
        &self,
        block_range: RangeInclusive<u32>,
        fragments: NonEmpty<Fragment>,
    ) -> Result<()> {
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
        fragment_ids: NonEmpty<NonNegative<i32>>,
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
    use itertools::Itertools;
    use ports::{
        storage::{Error, Storage},
        types::{nonempty, CollectNonEmpty},
    };
    use rand::{thread_rng, Rng, SeedableRng};

    use super::*;

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
        // given
        let storage = start_db().await;
        let latest_height = random_non_zero_height();

        let latest_submission = given_incomplete_submission(latest_height);
        storage.insert(latest_submission.clone()).await.unwrap();

        let older_submission = given_incomplete_submission(latest_height - 1);
        storage.insert(older_submission).await.unwrap();

        // when
        let actual = storage.submission_w_latest_block().await.unwrap().unwrap();

        // then
        assert_eq!(actual, latest_submission);
    }

    #[tokio::test]
    async fn can_update_completion_status() {
        // given
        let storage = start_db().await;

        let height = random_non_zero_height();
        let submission = given_incomplete_submission(height);
        let block_hash = submission.block_hash;
        storage.insert(submission).await.unwrap();

        // when
        let submission = storage.set_submission_completed(block_hash).await.unwrap();

        // then
        assert!(submission.completed);
    }

    #[tokio::test]
    async fn updating_a_missing_submission_causes_an_error() {
        // given
        let storage = start_db().await;

        let height = random_non_zero_height();
        let submission = given_incomplete_submission(height);
        let block_hash = submission.block_hash;

        // when
        let result = storage.set_submission_completed(block_hash).await;

        // then
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
    ) -> NonEmpty<NonNegative<i32>> {
        storage
            .insert_bundle_and_fragments(
                0..=0,
                nonempty!(
                    Fragment {
                        data: nonempty![0],
                        unused_bytes: 1000,
                        total_bytes: 100.try_into().unwrap()
                    },
                    Fragment {
                        data: nonempty![1],
                        unused_bytes: 1000,
                        total_bytes: 100.try_into().unwrap()
                    }
                ),
            )
            .await
            .unwrap();

        storage
            .oldest_nonfinalized_fragments(0, 2)
            .await
            .unwrap()
            .into_iter()
            .map(|f| f.id)
            .collect_nonempty()
            .unwrap()
    }

    #[tokio::test]
    async fn can_record_and_get_pending_txs() {
        // given
        let storage = start_db().await;

        let fragment_ids = ensure_some_fragments_exists_in_the_db(&storage).await;

        let tx_hash = rand::random::<[u8; 32]>();
        storage
            .record_pending_tx(tx_hash, fragment_ids)
            .await
            .unwrap();

        // when
        let has_pending = storage.has_pending_txs().await.unwrap();
        let pending_txs = storage.get_pending_txs().await.unwrap();

        // then
        assert!(has_pending);
        assert_eq!(pending_txs.len(), 1);
        assert_eq!(pending_txs[0].hash, tx_hash);
        assert_eq!(pending_txs[0].state, TransactionState::Pending);
    }

    #[tokio::test]
    async fn can_update_tx_state() {
        // given
        let storage = start_db().await;

        let fragment_ids = ensure_some_fragments_exists_in_the_db(&storage).await;
        let tx_hash = rand::random::<[u8; 32]>();
        storage
            .record_pending_tx(tx_hash, fragment_ids)
            .await
            .unwrap();

        // when
        storage
            .update_tx_state(tx_hash, TransactionState::Finalized(Utc::now()))
            .await
            .unwrap();

        // then
        let has_pending = storage.has_pending_txs().await.unwrap();
        let pending_txs = storage.get_pending_txs().await.unwrap();

        assert!(!has_pending);
        assert!(pending_txs.is_empty());
    }

    #[tokio::test]
    async fn can_insert_bundle_and_fragments() {
        // given
        let storage = start_db().await;

        let block_range = 1..=5;
        let fragment_1 = Fragment {
            data: nonempty![1u8, 2, 3],
            unused_bytes: 1000,
            total_bytes: 100.try_into().unwrap(),
        };
        let fragment_2 = Fragment {
            data: nonempty![4u8, 5, 6],
            unused_bytes: 1000,
            total_bytes: 100.try_into().unwrap(),
        };
        let fragments = nonempty![fragment_1.clone(), fragment_2.clone()];

        // when
        storage
            .insert_bundle_and_fragments(block_range.clone(), fragments.clone())
            .await
            .unwrap();

        // then
        let inserted_fragments = storage
            .oldest_nonfinalized_fragments(0, 2)
            .await
            .unwrap()
            .into_iter()
            .collect_vec();

        assert_eq!(inserted_fragments.len(), 2);
        for (inserted_fragment, given_fragment) in inserted_fragments.iter().zip(fragments.iter()) {
            assert_eq!(inserted_fragment.fragment, *given_fragment);
        }
    }

    fn round_to_millis(date: DateTime<Utc>) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(date.timestamp_millis()).unwrap()
    }

    #[tokio::test]
    async fn can_get_last_time_a_fragment_was_finalized() {
        // given
        let storage = start_db().await;

        let fragment_ids = ensure_some_fragments_exists_in_the_db(&storage).await;
        let tx_hash = rand::random::<[u8; 32]>();
        storage
            .record_pending_tx(tx_hash, fragment_ids)
            .await
            .unwrap();

        let finalization_time = Utc::now();

        // when
        storage
            .update_tx_state(tx_hash, TransactionState::Finalized(finalization_time))
            .await
            .unwrap();

        // then
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
                let block_data = nonempty![height as u8];
                ports::storage::FuelBlock {
                    hash: block_hash,
                    height,
                    data: block_data,
                }
            })
            .collect_nonempty()
            .expect("shouldn't be empty");

        storage.insert_blocks(blocks).await.unwrap();
    }

    async fn insert_sequence_of_bundled_blocks(
        storage: impl Storage,
        range: RangeInclusive<u32>,
        num_fragments: usize,
    ) {
        insert_sequence_of_unbundled_blocks(&storage, range.clone()).await;

        let fragments = std::iter::repeat(Fragment {
            data: nonempty![0],
            unused_bytes: 1000,
            total_bytes: 100.try_into().unwrap(),
        })
        .take(num_fragments)
        .collect_nonempty()
        .unwrap();

        storage
            .insert_bundle_and_fragments(range, fragments)
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
        // given
        let storage = start_db().await;

        // Insert blocks 1 to 10
        insert_sequence_of_unbundled_blocks(&storage, 1..=10).await;

        // when
        let height_range = lowest_unbundled_sequence(&storage, 0, usize::MAX).await;

        // then
        assert_eq!(height_range, 1..=10);
    }

    #[tokio::test]
    async fn handles_holes_in_sequences() {
        // given
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
        // given
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(&storage, 0..=10).await;

        // when
        let height_range = lowest_unbundled_sequence(&storage, 2, usize::MAX).await;

        // then
        assert_eq!(height_range, 2..=10);
    }

    #[tokio::test]
    async fn respects_limit() {
        // given
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(&storage, 0..=10).await;

        // when
        let height_range = lowest_unbundled_sequence(&storage, 0, 2).await;

        // then
        assert_eq!(height_range, 0..=1);
    }

    #[tokio::test]
    async fn ignores_bundled_blocks() {
        // given
        let storage = start_db().await;

        insert_sequence_of_bundled_blocks(&storage, 0..=2, 1).await;
        insert_sequence_of_unbundled_blocks(&storage, 3..=4).await;

        // when
        let height_range = lowest_unbundled_sequence(&storage, 0, usize::MAX).await;

        // then
        assert_eq!(height_range, 3..=4);
    }

    /// This can happen if we change the lookback config a couple of times in a short period of time
    #[tokio::test]
    async fn can_handle_bundled_blocks_appearing_after_unbundled_ones() {
        // given
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(&storage, 0..=2).await;
        insert_sequence_of_bundled_blocks(&storage, 7..=10, 1).await;
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
        insert_sequence_of_bundled_blocks(&storage, 0..=u16::MAX as u32 * 2, u16::MAX as usize * 2)
            .await;
    }

    #[tokio::test]
    async fn excludes_fragments_from_bundles_ending_before_starting_height() {
        // given
        let storage = start_db().await;
        let starting_height = 10;

        // Insert a bundle that ends before the starting_height
        storage
            .insert_bundle_and_fragments(
                1..=5, // Bundle ends at 5
                nonempty!(Fragment {
                    data: nonempty![0],
                    unused_bytes: 1000,
                    total_bytes: 100.try_into().unwrap()
                }),
            )
            .await
            .unwrap();

        // Insert a bundle that ends after the starting_height
        let fragment = Fragment {
            data: nonempty![1],
            unused_bytes: 1000,
            total_bytes: 100.try_into().unwrap(),
        };
        storage
            .insert_bundle_and_fragments(
                10..=15, // Bundle ends at 15
                nonempty!(fragment.clone()),
            )
            .await
            .unwrap();

        // when
        let fragments = storage
            .oldest_nonfinalized_fragments(starting_height, 10)
            .await
            .unwrap();

        // then
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].fragment, fragment);
    }

    #[tokio::test]
    async fn includes_fragments_from_bundles_ending_at_starting_height() {
        // given
        let storage = start_db().await;
        let starting_height = 10;

        // Insert a bundle that ends exactly at the starting_height
        let fragment = Fragment {
            data: nonempty![2],
            unused_bytes: 1000,
            total_bytes: 100.try_into().unwrap(),
        };
        storage
            .insert_bundle_and_fragments(
                5..=10, // Bundle ends at 10
                nonempty!(fragment.clone()),
            )
            .await
            .unwrap();

        // when
        let fragments = storage
            .oldest_nonfinalized_fragments(starting_height, 10)
            .await
            .unwrap();

        // then
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].fragment, fragment);
    }

    #[tokio::test]
    async fn empty_db_reports_missing_heights() -> Result<()> {
        // given
        let current_height = 10;
        let storage = start_db().await;

        // when
        let missing_blocks = storage.missing_blocks(0, current_height).await?;

        // then
        assert_eq!(missing_blocks, vec![0..=current_height]);

        Ok(())
    }

    #[tokio::test]
    async fn missing_blocks_no_holes() -> Result<()> {
        // given
        let current_height = 10;
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(&storage, 0..=5).await;

        // when
        let missing_blocks = storage.missing_blocks(0, current_height).await?;

        // then
        assert_eq!(missing_blocks, vec![6..=current_height]);

        Ok(())
    }

    #[tokio::test]
    async fn reports_holes_in_blocks() -> Result<()> {
        // given
        let current_height = 15;
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(&storage, 3..=5).await;
        insert_sequence_of_unbundled_blocks(&storage, 8..=10).await;

        // when
        let missing_blocks = storage.missing_blocks(0, current_height).await?;

        // then
        assert_eq!(missing_blocks, vec![0..=2, 6..=7, 11..=current_height]);

        Ok(())
    }
}
