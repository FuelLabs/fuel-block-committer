use std::{
    fmt::{Display, Formatter},
    iter::{Chain, Once},
    num::NonZeroUsize,
    ops::{Index, RangeInclusive},
    sync::Arc,
};

use delegate::delegate;
pub use futures::stream::BoxStream;
use itertools::Itertools;
pub use sqlx::types::chrono::{DateTime, Utc};

use crate::types::{
    BlockSubmission, BlockSubmissionTx, CollectNonEmpty, Fragment, L1Tx, NonEmpty, NonNegative,
    TransactionState,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("db response: {0}")]
    Database(String),
    #[error("data conversion app<->db failed: {0}")]
    Conversion(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuelBlock {
    pub hash: [u8; 32],
    pub height: u32,
    pub data: NonEmpty<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFragment {
    pub id: NonNegative<i32>,
    pub idx: NonNegative<i32>,
    pub bundle_id: NonNegative<i32>,
    pub fragment: Fragment,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequentialFuelBlocks {
    blocks: NonEmpty<FuelBlock>,
}

impl IntoIterator for SequentialFuelBlocks {
    type Item = FuelBlock;
    type IntoIter = Chain<Once<Self::Item>, std::vec::IntoIter<Self::Item>>;
    fn into_iter(self) -> Self::IntoIter {
        self.blocks.into_iter()
    }
}

impl Index<usize> for SequentialFuelBlocks {
    type Output = FuelBlock;
    fn index(&self, index: usize) -> &Self::Output {
        &self.blocks[index]
    }
}

impl SequentialFuelBlocks {
    pub fn into_inner(self) -> NonEmpty<FuelBlock> {
        self.blocks
    }

    pub fn from_first_sequence(blocks: NonEmpty<FuelBlock>) -> Self {
        let blocks = blocks
            .into_iter()
            .scan(None, |prev, block| match prev {
                Some(height) if *height + 1 == block.height => {
                    *prev = Some(block.height);
                    Some(block)
                }
                None => {
                    *prev = Some(block.height);
                    Some(block)
                }
                _ => None,
            })
            .collect_nonempty()
            .expect("at least the first block");

        blocks.try_into().expect("blocks are sequential")
    }

    pub fn len(&self) -> NonZeroUsize {
        self.blocks.len_nonzero()
    }

    pub fn height_range(&self) -> RangeInclusive<u32> {
        let first = self.blocks.first().height;
        let last = self.blocks.last().height;
        first..=last
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSequence {
    reason: String,
}

impl InvalidSequence {
    pub fn new(reason: String) -> Self {
        Self { reason }
    }
}

impl Display for InvalidSequence {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "invalid sequence: {}", self.reason)
    }
}

impl std::error::Error for InvalidSequence {}

impl TryFrom<NonEmpty<FuelBlock>> for SequentialFuelBlocks {
    type Error = InvalidSequence;

    fn try_from(blocks: NonEmpty<FuelBlock>) -> std::result::Result<Self, Self::Error> {
        let is_sorted = blocks
            .iter()
            .tuple_windows()
            .all(|(l, r)| l.height < r.height);

        if !is_sorted {
            return Err(InvalidSequence::new(
                "blocks are not sorted by height".to_string(),
            ));
        }

        let is_sequential = blocks
            .iter()
            .tuple_windows()
            .all(|(l, r)| l.height + 1 == r.height);
        if !is_sequential {
            return Err(InvalidSequence::new(
                "blocks are not sequential by height".to_string(),
            ));
        }

        Ok(Self { blocks })
    }
}

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
pub trait Storage: Send + Sync {
    async fn record_block_submission(
        &self,
        submission_tx: BlockSubmissionTx,
        submission: BlockSubmission,
    ) -> Result<NonNegative<i32>>;
    async fn get_pending_block_submission_txs(
        &self,
        submission_id: NonNegative<i32>,
    ) -> Result<Vec<BlockSubmissionTx>>;
    async fn update_block_submission_tx(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<BlockSubmission>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    async fn insert_blocks(&self, block: NonEmpty<FuelBlock>) -> Result<()>;
    async fn missing_blocks(
        &self,
        starting_height: u32,
        current_height: u32,
    ) -> Result<Vec<RangeInclusive<u32>>>;
    async fn lowest_sequence_of_unbundled_blocks(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Option<SequentialFuelBlocks>>;
    async fn insert_bundle_and_fragments(
        &self,
        block_range: RangeInclusive<u32>,
        fragments: NonEmpty<Fragment>,
    ) -> Result<()>;

    async fn record_pending_tx(
        &self,
        tx: L1Tx,
        fragments: NonEmpty<NonNegative<i32>>,
    ) -> Result<()>;
    async fn get_pending_txs(&self) -> Result<Vec<L1Tx>>;
    async fn has_pending_txs(&self) -> Result<bool>;
    async fn oldest_nonfinalized_fragments(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Vec<BundleFragment>>;
    async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>>;
    async fn update_tx_state(&self, hash: [u8; 32], state: TransactionState) -> Result<()>;
}

impl<T: Storage + Send + Sync> Storage for Arc<T> {
    delegate! {
        to (**self) {
                async fn record_block_submission(&self, submission_tx: BlockSubmissionTx, submission: BlockSubmission) -> Result<NonNegative<i32>>;
                async fn get_pending_block_submission_txs(&self, submission_id: NonNegative<i32>) -> Result<Vec<BlockSubmissionTx>>;
                async fn update_block_submission_tx(&self, hash: [u8; 32], state: TransactionState) -> Result<BlockSubmission>;
                async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
                async fn insert_blocks(&self, block: NonEmpty<FuelBlock>) -> Result<()>;
                async fn missing_blocks(
                    &self,
                    starting_height: u32,
                    current_height: u32,
                ) -> Result<Vec<RangeInclusive<u32>>>;
                async fn lowest_sequence_of_unbundled_blocks(
                    &self,
                    starting_height: u32,
                    limit: usize,
                ) -> Result<Option<SequentialFuelBlocks>>;
                async fn insert_bundle_and_fragments(
                    &self,
                    block_range: RangeInclusive<u32>,
                    fragments: NonEmpty<Fragment>,
                ) -> Result<()>;
                async fn record_pending_tx(
                    &self,
                    tx: L1Tx,
                    fragment_id: NonEmpty<NonNegative<i32>>,
                ) -> Result<()>;
                async fn get_pending_txs(&self) -> Result<Vec<L1Tx>>;
                async fn has_pending_txs(&self) -> Result<bool>;
                async fn oldest_nonfinalized_fragments(
                    &self,
                    starting_height: u32,
                    limit: usize,
                ) -> Result<Vec<BundleFragment>>;
                async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>>;
                async fn update_tx_state(&self, hash: [u8; 32], state: TransactionState) -> Result<()>;
        }
    }
}

impl<T: Storage + Send + Sync> Storage for &T {
    delegate! {
        to (**self) {
                async fn record_block_submission(&self, submission_tx: BlockSubmissionTx, submission: BlockSubmission) -> Result<NonNegative<i32>>;
                async fn get_pending_block_submission_txs(&self, submission_id: NonNegative<i32>) -> Result<Vec<BlockSubmissionTx>>;
                async fn update_block_submission_tx(&self, hash: [u8; 32], state: TransactionState) -> Result<BlockSubmission>;
                async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
                async fn insert_blocks(&self, block: NonEmpty<FuelBlock>) -> Result<()>;
                async fn missing_blocks(
                    &self,
                    starting_height: u32,
                    current_height: u32,
                ) -> Result<Vec<RangeInclusive<u32>>>;
                async fn lowest_sequence_of_unbundled_blocks(
                    &self,
                    starting_height: u32,
                    limit: usize,
                ) -> Result<Option<SequentialFuelBlocks>>;
                async fn insert_bundle_and_fragments(
                    &self,
                    block_range: RangeInclusive<u32>,
                    fragments: NonEmpty<Fragment>,
                ) -> Result<()>;
                async fn record_pending_tx(
                    &self,
                    tx: L1Tx,
                    fragment_id: NonEmpty<NonNegative<i32>>,
                ) -> Result<()>;
                async fn get_pending_txs(&self) -> Result<Vec<L1Tx>>;
                async fn has_pending_txs(&self) -> Result<bool>;
                async fn oldest_nonfinalized_fragments(
                    &self,
                    starting_height: u32,
                    limit: usize,
                ) -> Result<Vec<BundleFragment>>;
                async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>>;
                async fn update_tx_state(&self, hash: [u8; 32], state: TransactionState) -> Result<()>;
        }
    }
}

#[cfg(test)]
mod tests {
    use nonempty::{nonempty, NonEmpty};

    use super::*;

    fn create_fuel_block(height: u32) -> FuelBlock {
        let mut hash = [0; 32];
        hash[..4].copy_from_slice(&height.to_be_bytes());

        FuelBlock {
            hash,
            height,
            data: nonempty![0u8],
        }
    }

    fn create_non_empty_fuel_blocks(block_heights: &[u32]) -> NonEmpty<FuelBlock> {
        block_heights
            .iter()
            .cloned()
            .map(create_fuel_block)
            .collect_nonempty()
            .unwrap()
    }

    // Test: Successful conversion from a valid, sequential list of FuelBlocks
    #[test]
    fn try_from_with_valid_sequential_blocks_returns_ok() {
        // given
        let blocks = create_non_empty_fuel_blocks(&[1, 2, 3, 4, 5]);

        // when
        let seq_blocks = SequentialFuelBlocks::try_from(blocks.clone());

        // then
        assert!(
            seq_blocks.is_ok(),
            "Conversion should succeed for sequential blocks"
        );
        let seq_blocks = seq_blocks.unwrap();
        assert_eq!(
            seq_blocks.blocks, blocks,
            "SequentialFuelBlocks should contain the original blocks"
        );
    }

    // Test: Conversion fails when blocks are not sorted by height
    #[test]
    fn try_from_with_non_sorted_blocks_returns_error() {
        // given
        let blocks = create_non_empty_fuel_blocks(&[1, 3, 2, 4, 5]);

        // when
        let seq_blocks = SequentialFuelBlocks::try_from(blocks.clone());

        // then
        assert!(
            seq_blocks.is_err(),
            "Conversion should fail for non-sorted blocks"
        );
        let error = seq_blocks.unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid sequence: blocks are not sorted by height",
            "Error message should indicate sorting issue"
        );
    }

    // Test: Conversion fails when blocks have gaps in their heights
    #[test]
    fn try_from_with_non_sequential_blocks_returns_error() {
        // given
        let blocks = create_non_empty_fuel_blocks(&[1, 2, 4, 5]);

        // when
        let seq_blocks = SequentialFuelBlocks::try_from(blocks.clone());

        // then
        assert!(
            seq_blocks.is_err(),
            "Conversion should fail for non-sequential blocks"
        );
        let error = seq_blocks.unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid sequence: blocks are not sequential by height",
            "Error message should indicate sequentiality issue"
        );
    }

    // Test: Iterating over SequentialFuelBlocks yields all blocks in order
    #[test]
    fn iterates_over_sequential_fuel_blocks_correctly() {
        // given
        let blocks = create_non_empty_fuel_blocks(&[10, 11, 12]);
        let seq_blocks = SequentialFuelBlocks::try_from(blocks.clone()).unwrap();

        // when
        let collected: Vec<FuelBlock> = seq_blocks.clone().into_iter().collect();

        // then
        assert_eq!(
            collected,
            vec![
                create_fuel_block(10),
                create_fuel_block(11),
                create_fuel_block(12)
            ],
            "Iterated blocks should match the original sequence"
        );
    }

    // Test: Indexing into SequentialFuelBlocks retrieves the correct FuelBlock
    #[test]
    fn indexing_returns_correct_fuel_block() {
        // given
        let blocks = create_non_empty_fuel_blocks(&[100, 101, 102, 103]);
        let seq_blocks = SequentialFuelBlocks::try_from(blocks.clone()).unwrap();

        // when & Then
        assert_eq!(
            seq_blocks[0],
            create_fuel_block(100),
            "First block should match"
        );
        assert_eq!(
            seq_blocks[1],
            create_fuel_block(101),
            "Second block should match"
        );
        assert_eq!(
            seq_blocks[3],
            create_fuel_block(103),
            "Fourth block should match"
        );
    }

    // Test: Accessing an out-of-bounds index panics as expected
    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn indexing_out_of_bounds_panics() {
        // given
        let blocks = create_non_empty_fuel_blocks(&[1, 2, 3]);
        let seq_blocks = SequentialFuelBlocks::try_from(blocks).unwrap();

        // when
        let _ = &seq_blocks[5];

        // then
        // Panic is expected
    }

    // Test: len method returns the correct number of blocks
    #[test]
    fn len_returns_correct_number_of_blocks() {
        // given
        let blocks = create_non_empty_fuel_blocks(&[7, 8, 9, 10]);
        let seq_blocks = SequentialFuelBlocks::try_from(blocks.clone()).unwrap();

        // when
        let length = seq_blocks.len();

        // then
        assert_eq!(
            length,
            NonZeroUsize::new(4).unwrap(),
            "Length should be equal to the number of blocks"
        );
    }

    // Test: height_range method returns the correct inclusive range
    #[test]
    fn height_range_returns_correct_range() {
        // given
        let blocks = create_non_empty_fuel_blocks(&[20, 21, 22, 23]);
        let seq_blocks = SequentialFuelBlocks::try_from(blocks.clone()).unwrap();

        // when
        let range = seq_blocks.height_range();

        // then
        assert_eq!(
            range,
            20..=23,
            "Height range should span from the first to the last block's height"
        );
    }

    // Test: from_first_sequence includes all blocks when they are sequential
    #[test]
    fn from_first_sequence_with_all_sequential_blocks_includes_all() {
        // given
        let blocks = create_non_empty_fuel_blocks(&[5, 6, 7, 8]);

        // when
        let seq_blocks = SequentialFuelBlocks::from_first_sequence(blocks.clone());

        // then
        assert_eq!(
            seq_blocks.blocks, blocks,
            "All sequential blocks should be included"
        );
    }

    // Test: from_first_sequence stops at the first gap in block heights
    #[test]
    fn from_first_sequence_with_gaps_includes_up_to_first_gap() {
        // given
        let blocks = create_non_empty_fuel_blocks(&[1, 2, 4, 5, 7]);

        // when
        let seq_blocks = SequentialFuelBlocks::from_first_sequence(blocks);

        // then
        let expected = nonempty![create_fuel_block(1), create_fuel_block(2)];
        assert_eq!(
            seq_blocks.blocks, expected,
            "Only blocks up to the first gap should be included"
        );
    }

    // Test: from_first_sequence correctly handles a single block
    #[test]
    fn from_first_sequence_with_single_block_includes_it() {
        // given
        let blocks = nonempty![create_fuel_block(42)];

        // when
        let seq_blocks = SequentialFuelBlocks::from_first_sequence(blocks.clone());

        // then
        assert_eq!(
            seq_blocks.blocks, blocks,
            "Single block should be included correctly"
        );
    }

    // Test: into_inner retrieves the original NonEmpty<FuelBlock>
    #[test]
    fn into_inner_returns_original_nonempty_blocks() {
        // given
        let blocks = create_non_empty_fuel_blocks(&[10, 11, 12]);
        let seq_blocks = SequentialFuelBlocks::try_from(blocks.clone()).unwrap();

        // when
        let inner = seq_blocks.into_inner();

        // then
        assert_eq!(
            inner, blocks,
            "into_inner should return the original NonEmpty<FuelBlock>"
        );
    }

    // Test: InvalidSequence error displays correctly
    #[test]
    fn invalid_sequence_display_formats_correctly() {
        // given
        let error = InvalidSequence::new("test reason".to_string());

        // when
        let display = error.to_string();

        // then
        assert_eq!(
            display, "invalid sequence: test reason",
            "Error display should match the expected format"
        );
    }

    // Test: Single block is always considered sequential
    #[test]
    fn single_block_is_always_sequential() {
        // given
        let blocks = nonempty![create_fuel_block(999)];

        // when
        let seq_blocks = SequentialFuelBlocks::try_from(blocks.clone());

        // then
        assert!(
            seq_blocks.is_ok(),
            "Single block should be considered sequential"
        );
        let seq_blocks = seq_blocks.unwrap();
        assert_eq!(
            seq_blocks.blocks, blocks,
            "SequentialFuelBlocks should contain the single block"
        );
    }

    // Test: Two blocks with the same height result in an error
    #[test]
    fn two_blocks_with_same_height_returns_error() {
        // given
        let blocks = nonempty![create_fuel_block(1), create_fuel_block(1)];

        // when
        let seq_blocks = SequentialFuelBlocks::try_from(blocks.clone());

        // then
        assert!(
            seq_blocks.is_err(),
            "Duplicate heights should result in an error"
        );
        let error = seq_blocks.unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid sequence: blocks are not sorted by height",
            "Error message should indicate sorting issue due to duplicate heights"
        );
    }

    // Test: Two blocks with non-consecutive heights result in an error
    #[test]
    fn two_blocks_with_non_consecutive_heights_returns_error() {
        // given
        let blocks = nonempty![create_fuel_block(1), create_fuel_block(3)];

        // when
        let seq_blocks = SequentialFuelBlocks::try_from(blocks.clone());

        // then
        assert!(
            seq_blocks.is_err(),
            "Non-consecutive heights should result in an error"
        );
        let error = seq_blocks.unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid sequence: blocks are not sequential by height",
            "Error message should indicate sequentiality issue"
        );
    }
}
