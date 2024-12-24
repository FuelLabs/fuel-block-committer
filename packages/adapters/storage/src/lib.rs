mod mappings;
#[cfg(feature = "test-helpers")]
mod test_instance;

use std::ops::RangeInclusive;

#[cfg(feature = "test-helpers")]
pub use test_instance::*;

pub(crate) mod error;
mod postgres;
pub use postgres::{DbConfig, Postgres};
use services::{
    types::{
        storage::{BundleFragment, SequentialFuelBlocks},
        BlockSubmission, BlockSubmissionTx, BundleCost, CompressedFuelBlock, DateTime, Fragment,
        L1Tx, NonEmpty, NonNegative, TransactionCostUpdate, TransactionState, Utc,
    },
    Result,
};

impl services::state_listener::port::Storage for Postgres {
    async fn get_non_finalized_txs(&self) -> Result<Vec<L1Tx>> {
        self._get_non_finalized_txs().await.map_err(Into::into)
    }

    async fn update_tx_states_and_costs(
        &self,
        selective_changes: Vec<([u8; 32], TransactionState)>,
        noncewide_changes: Vec<([u8; 32], u32, TransactionState)>,
        cost_per_tx: Vec<TransactionCostUpdate>,
    ) -> Result<()> {
        self._update_tx_states_and_costs(selective_changes, noncewide_changes, cost_per_tx)
            .await
            .map_err(Into::into)
    }

    async fn has_pending_txs(&self) -> Result<bool> {
        self._has_pending_txs().await.map_err(Into::into)
    }

    async fn earliest_submission_attempt(&self, nonce: u32) -> Result<Option<DateTime<Utc>>> {
        self._earliest_submission_attempt(nonce)
            .await
            .map_err(Into::into)
    }
}

impl services::cost_reporter::port::Storage for Postgres {
    async fn get_finalized_costs(
        &self,
        from_block_height: u32,
        limit: usize,
    ) -> Result<Vec<BundleCost>> {
        self._get_finalized_costs(from_block_height, limit)
            .await
            .map_err(Into::into)
    }

    async fn get_latest_costs(&self, limit: usize) -> Result<Vec<BundleCost>> {
        self._get_latest_costs(limit).await.map_err(Into::into)
    }
}

impl services::status_reporter::port::Storage for Postgres {
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>> {
        self._submission_w_latest_block().await.map_err(Into::into)
    }
}

impl services::block_importer::port::Storage for Postgres {
    async fn missing_blocks(
        &self,
        starting_height: u32,
        current_height: u32,
    ) -> Result<Vec<RangeInclusive<u32>>> {
        self._missing_blocks(starting_height, current_height)
            .await
            .map_err(Into::into)
    }

    async fn insert_blocks(&self, blocks: NonEmpty<CompressedFuelBlock>) -> Result<()> {
        Ok(self._insert_blocks(blocks).await?)
    }
}

impl services::block_bundler::port::Storage for Postgres {
    async fn lowest_sequence_of_unbundled_blocks(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Option<SequentialFuelBlocks>> {
        self._lowest_unbundled_blocks(starting_height, limit)
            .await
            .map_err(Into::into)
    }
    async fn insert_bundle_and_fragments(
        &self,
        bundle_id: NonNegative<i32>,
        block_range: RangeInclusive<u32>,
        fragments: NonEmpty<Fragment>,
    ) -> Result<()> {
        self._insert_bundle_and_fragments(bundle_id, block_range, fragments)
            .await
            .map_err(Into::into)
    }
    async fn next_bundle_id(&self) -> Result<NonNegative<i32>> {
        self._next_bundle_id().await.map_err(Into::into)
    }
}

impl services::block_committer::port::Storage for Postgres {
    async fn record_block_submission(
        &self,
        submission_tx: BlockSubmissionTx,
        submission: BlockSubmission,
        created_at: DateTime<Utc>,
    ) -> Result<NonNegative<i32>> {
        self._record_block_submission(submission_tx, submission, created_at)
            .await
            .map_err(Into::into)
    }
    async fn get_pending_block_submission_txs(
        &self,
        submission_id: NonNegative<i32>,
    ) -> Result<Vec<BlockSubmissionTx>> {
        self._get_pending_block_submission_txs(submission_id)
            .await
            .map_err(Into::into)
    }
    async fn update_block_submission_tx(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<BlockSubmission> {
        self._update_block_submission_tx(hash, state)
            .await
            .map_err(Into::into)
    }
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>> {
        self._submission_w_latest_block().await.map_err(Into::into)
    }
}

impl services::state_committer::port::Storage for Postgres {
    async fn has_nonfinalized_txs(&self) -> Result<bool> {
        self._has_nonfinalized_txs().await.map_err(Into::into)
    }
    async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>> {
        self._last_time_a_fragment_was_finalized()
            .await
            .map_err(Into::into)
    }
    async fn record_pending_tx(
        &self,
        tx: L1Tx,
        fragment_ids: NonEmpty<NonNegative<i32>>,
        created_at: DateTime<Utc>,
    ) -> Result<()> {
        self._record_pending_tx(tx, fragment_ids, created_at)
            .await
            .map_err(Into::into)
    }
    async fn oldest_nonfinalized_fragments(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Vec<BundleFragment>> {
        self._oldest_nonfinalized_fragments(starting_height, limit)
            .await
            .map_err(Into::into)
    }
    async fn fragments_submitted_by_tx(&self, tx_hash: [u8; 32]) -> Result<Vec<BundleFragment>> {
        self._fragments_submitted_by_tx(tx_hash)
            .await
            .map_err(Into::into)
    }
    async fn get_latest_pending_txs(&self) -> Result<Option<services::types::L1Tx>> {
        self._get_latest_pending_txs().await.map_err(Into::into)
    }

    async fn latest_bundled_height(&self) -> Result<Option<u32>> {
        self._latest_bundled_height().await.map_err(Into::into)
    }
}

impl services::state_pruner::port::Storage for Postgres {
    async fn prune_entries_older_than(&self, date: DateTime<Utc>) -> Result<()> {
        self._prune_entries_older_than(date)
            .await
            .map_err(Into::into)
    }

    async fn table_sizes(&self) -> Result<services::state_pruner::port::TableSizes> {
        self._table_sizes().await.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clock::TestClock;
    use itertools::Itertools;
    use rand::{thread_rng, Rng};
    use services::types::{nonempty, CollectNonEmpty};

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
            id: None,
            block_hash: rand::random(),
            block_height: fuel_block_height,
            completed: false,
        }
    }

    async fn ensure_some_fragments_exists_in_the_db(
        storage: impl services::block_bundler::port::Storage + services::state_committer::port::Storage,
        range: RangeInclusive<u32>,
    ) -> NonEmpty<NonNegative<i32>> {
        let next_id = storage.next_bundle_id().await.unwrap();
        storage
            .insert_bundle_and_fragments(
                next_id,
                range,
                nonempty!(
                    Fragment {
                        data: nonempty![0],
                        unused_bytes: 100,
                        total_bytes: 1000.try_into().unwrap()
                    },
                    Fragment {
                        data: nonempty![1],
                        unused_bytes: 100,
                        total_bytes: 1000.try_into().unwrap()
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
    async fn can_record_and_find_latest_block() {
        use services::block_committer::port::Storage;

        // given
        let storage = start_db().await;
        let latest_height = random_non_zero_height();

        let latest_submission = given_incomplete_submission(latest_height);
        let submission_tx = given_pending_tx(0);
        let submission_id = storage
            .record_block_submission(
                submission_tx,
                latest_submission.clone(),
                TestClock::default().now(),
            )
            .await
            .unwrap();

        let older_submission = given_incomplete_submission(latest_height - 1);
        let submission_tx = given_pending_tx(1);
        storage
            .record_block_submission(submission_tx, older_submission, TestClock::default().now())
            .await
            .unwrap();

        // when
        let actual = storage.submission_w_latest_block().await.unwrap().unwrap();

        // then
        let expected = BlockSubmission {
            id: Some(submission_id),
            ..latest_submission
        };
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn can_record_and_find_pending_tx() {
        use services::block_committer::port::Storage;

        // given
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();
        let latest_height = random_non_zero_height();

        let latest_submission = given_incomplete_submission(latest_height);
        let submission_tx = given_pending_tx(0);

        let submission_id = db
            .record_block_submission(
                submission_tx.clone(),
                latest_submission.clone(),
                TestClock::default().now(),
            )
            .await
            .unwrap();

        // when
        let actual = db
            .get_pending_block_submission_txs(submission_id)
            .await
            .unwrap()
            .pop()
            .expect("pending tx to exist");

        // then
        let expected = BlockSubmissionTx {
            id: actual.id,
            created_at: actual.created_at,
            submission_id: Some(submission_id),
            ..submission_tx
        };
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn can_update_completion_status() {
        use services::block_committer::port::Storage;

        // given
        let storage = start_db().await;

        let height = random_non_zero_height();
        let submission = given_incomplete_submission(height);
        let submission_tx = given_pending_tx(0);

        let submission_id = storage
            .record_block_submission(submission_tx, submission, TestClock::default().now())
            .await
            .unwrap();

        // when
        let submission_tx = storage
            .get_pending_block_submission_txs(submission_id)
            .await
            .unwrap()
            .pop()
            .expect("pending tx to exist");
        storage
            .update_block_submission_tx(submission_tx.hash, TransactionState::Finalized(Utc::now()))
            .await
            .unwrap();

        // then
        let pending_txs = storage
            .get_pending_block_submission_txs(submission_id)
            .await
            .unwrap();
        assert!(pending_txs.is_empty());

        let submission = storage
            .submission_w_latest_block()
            .await
            .unwrap()
            .expect("submission to exist");
        assert!(submission.completed);
    }

    #[tokio::test]
    async fn updating_a_missing_submission_tx_causes_an_error() {
        use services::block_committer::port::Storage;

        // given
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();

        let submission_tx = given_pending_tx(0);

        // when
        let result = db
            .update_block_submission_tx(submission_tx.hash, TransactionState::Finalized(Utc::now()))
            .await;

        // then
        let Err(services::Error::Storage(msg)) = result else {
            panic!("should be storage error");
        };

        let tx_hash = hex::encode(submission_tx.hash);
        assert_eq!(
            msg,
            format!("Cannot update tx state! Tx with hash: `{tx_hash}` not found in DB.")
        );
    }

    #[tokio::test]
    async fn can_insert_bundle_and_fragments() {
        use services::{
            block_bundler::port::Storage, state_committer::port::Storage as CommitterStorage,
        };

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

        let next_id = storage.next_bundle_id().await.unwrap();
        // when
        storage
            .insert_bundle_and_fragments(next_id, block_range.clone(), fragments.clone())
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
        use services::state_committer::port::Storage;
        use services::state_listener::port::Storage as ListenerStorage;

        // given
        let storage = start_db().await;

        let fragment_ids = ensure_some_fragments_exists_in_the_db(storage.clone(), 0..=0).await;
        let tx = L1Tx {
            hash: rand::random::<[u8; 32]>(),
            ..Default::default()
        };
        let hash = tx.hash;
        let nonce = tx.nonce;

        storage
            .record_pending_tx(tx, fragment_ids, TestClock::default().now())
            .await
            .unwrap();

        let finalization_time = Utc::now();

        // when
        let changes = vec![(hash, nonce, TransactionState::Finalized(finalization_time))];
        storage
            .update_tx_states_and_costs(vec![], changes, vec![])
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
        storage: impl services::block_importer::port::Storage,
        range: RangeInclusive<u32>,
    ) {
        let blocks = range
            .clone()
            .map(|height| CompressedFuelBlock {
                height,
                data: nonempty![height as u8],
            })
            .collect_nonempty()
            .expect("shouldn't be empty");

        storage.insert_blocks(blocks).await.unwrap();
    }

    async fn insert_sequence_of_bundled_blocks(
        storage: impl services::block_bundler::port::Storage
            + services::block_importer::port::Storage
            + Clone,
        range: RangeInclusive<u32>,
        num_fragments: usize,
    ) {
        insert_sequence_of_unbundled_blocks(storage.clone(), range.clone()).await;

        let fragments = std::iter::repeat(Fragment {
            data: nonempty![0],
            unused_bytes: 1000,
            total_bytes: 100.try_into().unwrap(),
        })
        .take(num_fragments)
        .collect_nonempty()
        .unwrap();

        let next_id = storage.next_bundle_id().await.unwrap();
        storage
            .insert_bundle_and_fragments(next_id, range, fragments)
            .await
            .unwrap();
    }

    async fn lowest_unbundled_sequence(
        storage: impl services::block_bundler::port::Storage,
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
        insert_sequence_of_unbundled_blocks(storage.clone(), 1..=10).await;

        // when
        let height_range = lowest_unbundled_sequence(storage.clone(), 0, usize::MAX).await;

        // then
        assert_eq!(height_range, 1..=10);
    }

    #[tokio::test]
    async fn handles_holes_in_sequences() {
        // given
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(storage.clone(), 0..=2).await;
        insert_sequence_of_unbundled_blocks(storage.clone(), 4..=6).await;

        // when
        let height_range = lowest_unbundled_sequence(storage.clone(), 0, usize::MAX).await;

        // then
        assert_eq!(height_range, 0..=2);
    }

    #[tokio::test]
    async fn respects_starting_height() {
        // given
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(storage.clone(), 0..=10).await;

        // when
        let height_range = lowest_unbundled_sequence(storage.clone(), 2, usize::MAX).await;

        // then
        assert_eq!(height_range, 2..=10);
    }

    #[tokio::test]
    async fn respects_limit() {
        // given
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(storage.clone(), 0..=10).await;

        // when
        let height_range = lowest_unbundled_sequence(storage.clone(), 0, 2).await;

        // then
        assert_eq!(height_range, 0..=1);
    }

    fn given_pending_tx(nonce: u32) -> BlockSubmissionTx {
        BlockSubmissionTx {
            hash: [nonce as u8; 32],
            nonce,
            state: TransactionState::Pending,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn ignores_bundled_blocks() {
        // given
        let storage = start_db().await;

        insert_sequence_of_bundled_blocks(storage.clone(), 0..=2, 1).await;
        insert_sequence_of_unbundled_blocks(storage.clone(), 3..=4).await;

        // when
        let height_range = lowest_unbundled_sequence(storage.clone(), 0, usize::MAX).await;

        // then
        assert_eq!(height_range, 3..=4);
    }

    /// This can happen if we change the lookback config a couple of times in a short period of time
    #[tokio::test]
    async fn can_handle_bundled_blocks_appearing_after_unbundled_ones() {
        // given
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(storage.clone(), 0..=2).await;
        insert_sequence_of_bundled_blocks(storage.clone(), 7..=10, 1).await;
        insert_sequence_of_unbundled_blocks(storage.clone(), 11..=15).await;

        // when
        let height_range = lowest_unbundled_sequence(storage.clone(), 0, usize::MAX).await;

        // then
        assert_eq!(height_range, 0..=2);
    }

    // Important because sqlx panics if the bundle is too big
    #[tokio::test]
    async fn can_insert_big_batches() {
        let storage = start_db().await;

        // u16::MAX because of implementation details
        insert_sequence_of_bundled_blocks(
            storage.clone(),
            0..=u16::MAX as u32 * 2,
            u16::MAX as usize * 2,
        )
        .await;
    }

    #[tokio::test]
    async fn excludes_fragments_from_bundles_ending_before_starting_height() {
        use services::{
            block_bundler::port::Storage, state_committer::port::Storage as CommitterStorage,
        };

        // given
        let storage = start_db().await;
        let starting_height = 10;

        // Insert a bundle that ends before the starting_height
        let next_id = storage.next_bundle_id().await.unwrap();
        storage
            .insert_bundle_and_fragments(
                next_id,
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

        let next_id = storage.next_bundle_id().await.unwrap();
        storage
            .insert_bundle_and_fragments(
                next_id,
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
        use services::{
            block_bundler::port::Storage, state_committer::port::Storage as CommitterStorage,
        };

        // given
        let storage = start_db().await;
        let starting_height = 10;

        // Insert a bundle that ends exactly at the starting_height
        let fragment = Fragment {
            data: nonempty![2],
            unused_bytes: 1000,
            total_bytes: 100.try_into().unwrap(),
        };
        let next_id = storage.next_bundle_id().await.unwrap();
        storage
            .insert_bundle_and_fragments(
                next_id,
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
    async fn can_get_next_bundle_id() {
        use services::{
            block_bundler::port::Storage, state_committer::port::Storage as CommitterStorage,
        };

        // given
        let storage = start_db().await;
        let starting_height = 10;

        // Insert a bundle that ends exactly at the starting_height
        let fragment = Fragment {
            data: nonempty![2],
            unused_bytes: 1000,
            total_bytes: 100.try_into().unwrap(),
        };
        let next_id = storage.next_bundle_id().await.unwrap();
        storage
            .insert_bundle_and_fragments(
                next_id,
                5..=10, // Bundle ends at 10
                nonempty!(fragment.clone()),
            )
            .await
            .unwrap();
        let fragments = storage
            .oldest_nonfinalized_fragments(starting_height, 10)
            .await
            .unwrap();

        // when
        let next_id = storage.next_bundle_id().await.unwrap();

        // then
        assert_eq!(next_id.get(), fragments[0].id.get() + 1);
    }

    #[tokio::test]
    async fn empty_db_reports_missing_heights() -> Result<()> {
        use services::block_importer::port::Storage;

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
        use services::block_importer::port::Storage;

        // given
        let current_height = 10;
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(storage.clone(), 0..=5).await;

        // when
        let missing_blocks = storage.missing_blocks(0, current_height).await?;

        // then
        assert_eq!(missing_blocks, vec![6..=current_height]);

        Ok(())
    }

    #[tokio::test]
    async fn reports_holes_in_blocks() -> Result<()> {
        use services::block_importer::port::Storage;

        // given
        let current_height = 15;
        let storage = start_db().await;

        insert_sequence_of_unbundled_blocks(storage.clone(), 3..=5).await;
        insert_sequence_of_unbundled_blocks(storage.clone(), 8..=10).await;

        // when
        let missing_blocks = storage.missing_blocks(0, current_height).await?;

        // then
        assert_eq!(missing_blocks, vec![0..=2, 6..=7, 11..=current_height]);

        Ok(())
    }

    #[tokio::test]
    async fn can_retrieve_fragments_submitted_by_tx() -> Result<()> {
        use services::state_committer::port::Storage;

        // given
        let storage = start_db().await;

        let fragment_ids = ensure_some_fragments_exists_in_the_db(storage.clone(), 0..=0).await;
        let hash = rand::random::<[u8; 32]>();
        let tx = L1Tx {
            hash,
            ..Default::default()
        };
        storage
            .record_pending_tx(tx, fragment_ids, TestClock::default().now())
            .await?;

        // when
        let fragments = storage.fragments_submitted_by_tx(hash).await?;

        // then
        assert_eq!(fragments.len(), 2);

        Ok(())
    }

    #[tokio::test]
    async fn can_get_latest_pending_txs() -> Result<()> {
        use services::state_committer::port::Storage;

        // given
        let storage = start_db().await;

        let fragment_ids = ensure_some_fragments_exists_in_the_db(storage.clone(), 0..=0).await;
        let (fragment_1, fragment_2) = (fragment_ids[0], fragment_ids[1]);
        let inserted_1 = L1Tx {
            hash: rand::random::<[u8; 32]>(),
            ..Default::default()
        };
        let mut inserted_2 = L1Tx {
            hash: rand::random::<[u8; 32]>(),
            nonce: 1,
            max_fee: 2000000000000,
            priority_fee: 1500000000,
            blob_fee: 100,
            ..Default::default()
        };

        let test_clock = TestClock::default();
        let now = test_clock.now();
        storage
            .record_pending_tx(inserted_1, nonempty![fragment_1], now)
            .await?;

        test_clock.advance_time(Duration::from_millis(1));
        let now = test_clock.now();
        storage
            .record_pending_tx(inserted_2.clone(), nonempty![fragment_2], now)
            .await?;

        // when
        let retrieved = storage.get_latest_pending_txs().await?.unwrap();

        // then
        inserted_2.id = retrieved.id;
        inserted_2.created_at = retrieved.created_at;
        assert_eq!(retrieved, inserted_2);

        Ok(())
    }

    #[tokio::test]
    async fn can_update_costs() -> Result<()> {
        use services::cost_reporter::port::Storage;
        use services::state_committer::port::Storage as StateStorage;
        use services::state_listener::port::Storage as ListenerStorage;

        // given
        let storage = start_db().await;

        let fragment_ids = ensure_some_fragments_exists_in_the_db(storage.clone(), 0..=0).await;
        let tx = L1Tx {
            hash: rand::random::<[u8; 32]>(),
            ..Default::default()
        };
        let hash = tx.hash;
        let nonce = tx.nonce;

        storage
            .record_pending_tx(tx, fragment_ids, TestClock::default().now())
            .await
            .unwrap();

        let finalization_time = Utc::now();

        // when
        let changes = vec![(hash, nonce, TransactionState::Finalized(finalization_time))];
        let cost_per_tx = TransactionCostUpdate {
            tx_hash: hash,
            total_fee: 1000u128,
            da_block_height: 5000u64,
        };
        storage
            .update_tx_states_and_costs(vec![], changes, vec![cost_per_tx.clone()])
            .await
            .unwrap();

        // then
        let bundle_cost = storage.get_finalized_costs(0, 10).await?;

        assert_eq!(bundle_cost.len(), 1);
        assert_eq!(bundle_cost[0].cost, cost_per_tx.total_fee);
        assert_eq!(bundle_cost[0].da_block_height, cost_per_tx.da_block_height);

        Ok(())
    }

    async fn ensure_fragments_have_transaction(
        storage: impl services::state_listener::port::Storage + services::state_committer::port::Storage,
        fragment_ids: NonEmpty<NonNegative<i32>>,
        state: TransactionState,
    ) -> [u8; 32] {
        let tx_hash = rand::random::<[u8; 32]>();
        let tx = L1Tx {
            hash: tx_hash,
            nonce: rand::random(),
            ..Default::default()
        };
        storage
            .record_pending_tx(tx.clone(), fragment_ids, TestClock::default().now())
            .await
            .unwrap();

        let changes = vec![(tx.hash, tx.nonce, state)];
        storage
            .update_tx_states_and_costs(vec![], changes, vec![])
            .await
            .expect("tx state should update");

        tx_hash
    }

    async fn ensure_finalized_fragments_exist_in_the_db(
        storage: impl services::block_bundler::port::Storage
            + services::state_committer::port::Storage
            + services::state_listener::port::Storage
            + Clone,
        range: RangeInclusive<u32>,
        total_fee: u128,
        da_block_height: u64,
    ) -> NonEmpty<NonNegative<i32>> {
        let fragment_in_db = ensure_some_fragments_exists_in_the_db(storage.clone(), range).await;

        let state = TransactionState::Finalized(Utc::now());
        let tx_hash =
            ensure_fragments_have_transaction(storage.clone(), fragment_in_db.clone(), state).await;

        let cost_per_tx = TransactionCostUpdate {
            tx_hash,
            total_fee,
            da_block_height,
        };
        storage
            .update_tx_states_and_costs(vec![], vec![], vec![cost_per_tx])
            .await
            .expect("cost update shouldn't fail");

        fragment_in_db
    }

    #[tokio::test]
    async fn costs_returned_only_for_finalized_bundles() {
        use services::cost_reporter::port::Storage;

        // given
        let storage = start_db().await;
        let cost = 1000u128;
        let da_height = 5000u64;
        let bundle_range = 1..=2;

        ensure_finalized_fragments_exist_in_the_db(
            storage.clone(),
            bundle_range.clone(),
            cost,
            da_height,
        )
        .await;

        // add submitted and unsubmitted fragments
        let fragment_ids = ensure_some_fragments_exists_in_the_db(storage.clone(), 3..=5).await;
        ensure_fragments_have_transaction(
            storage.clone(),
            fragment_ids,
            TransactionState::IncludedInBlock,
        )
        .await;
        ensure_some_fragments_exists_in_the_db(storage.clone(), 6..=10).await;

        // when
        let costs = storage.get_finalized_costs(0, 10).await.unwrap();

        // then
        assert_eq!(costs.len(), 1);

        let bundle_cost = &costs[0];
        assert_eq!(bundle_cost.start_height, *bundle_range.start() as u64);
        assert_eq!(bundle_cost.end_height, *bundle_range.end() as u64);
        assert_eq!(bundle_cost.cost, cost);
        assert_eq!(bundle_cost.da_block_height, da_height);
    }

    #[tokio::test]
    async fn costs_returned_only_for_finalized_with_replacement_txs() {
        use services::cost_reporter::port::Storage;

        // given
        let storage = start_db().await;
        let cost = 1000u128;
        let da_height = 5000u64;
        let bundle_range = 1..=2;

        let fragment_ids = ensure_finalized_fragments_exist_in_the_db(
            storage.clone(),
            bundle_range.clone(),
            cost,
            da_height,
        )
        .await;
        // simulate replaced txs
        ensure_fragments_have_transaction(
            storage.clone(),
            fragment_ids,
            TransactionState::SqueezedOut,
        )
        .await;
        ensure_some_fragments_exists_in_the_db(storage.clone(), 6..=10).await;

        // when
        let costs = storage.get_finalized_costs(0, 10).await.unwrap();

        // then
        assert_eq!(costs.len(), 1);

        let bundle_cost = &costs[0];
        assert_eq!(bundle_cost.start_height, *bundle_range.start() as u64);
        assert_eq!(bundle_cost.end_height, *bundle_range.end() as u64);
        assert_eq!(bundle_cost.cost, cost);
        assert_eq!(bundle_cost.da_block_height, da_height);
    }

    #[tokio::test]
    async fn respects_from_block_height_and_limit_in_get_finalized_costs() -> Result<()> {
        use services::cost_reporter::port::Storage;

        // given
        let storage = start_db().await;

        for i in 0..5 {
            let start_height = i * 10 + 1;
            let end_height = start_height + 9;
            let block_range = start_height..=end_height;

            ensure_finalized_fragments_exist_in_the_db(
                storage.clone(),
                block_range,
                1000u128,
                5000u64,
            )
            .await;
        }

        // when
        let from_block_height = 21;
        let limit = 2;
        let finalized_costs = storage
            .get_finalized_costs(from_block_height, limit)
            .await?;

        // then
        assert_eq!(finalized_costs.len(), 2);

        for bc in &finalized_costs {
            assert!(bc.start_height >= from_block_height as u64);
        }

        Ok(())
    }

    #[tokio::test]
    async fn get_finalized_costs_from_middle_of_range() -> Result<()> {
        use services::cost_reporter::port::Storage;

        // given
        let storage = start_db().await;

        for i in 0..5 {
            let start_height = i * 10 + 1;
            let end_height = start_height + 9;
            let block_range = start_height..=end_height;

            ensure_finalized_fragments_exist_in_the_db(
                storage.clone(),
                block_range,
                1000u128,
                5000u64,
            )
            .await;
        }

        // when
        let from_block_height = 25;
        let limit = 3;
        let finalized_costs = storage
            .get_finalized_costs(from_block_height, limit)
            .await?;

        // then
        assert_eq!(finalized_costs.len(), 3);

        assert_eq!(finalized_costs[0].start_height, 21);
        assert_eq!(finalized_costs[1].start_height, 31);
        assert_eq!(finalized_costs[2].start_height, 41);

        Ok(())
    }

    #[tokio::test]
    async fn get_latest_finalized_costs() -> Result<()> {
        use services::cost_reporter::port::Storage;

        // given
        let storage = start_db().await;

        for i in 0..5 {
            let start_height = i * 10 + 1;
            let end_height = start_height + 9;
            let block_range = start_height..=end_height;

            ensure_finalized_fragments_exist_in_the_db(
                storage.clone(),
                block_range,
                1000u128,
                5000u64,
            )
            .await;
        }

        // when
        let finalized_costs = storage.get_latest_costs(1).await?;

        // then
        assert_eq!(finalized_costs.len(), 1);
        let finalized_cost = &finalized_costs[0];

        assert_eq!(finalized_cost.start_height, 41);
        assert_eq!(finalized_cost.end_height, 50);

        Ok(())
    }
}
