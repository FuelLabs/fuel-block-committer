use delegate::delegate;
use std::{
    fmt::{Display, Formatter},
    num::NonZeroUsize,
    ops::{Deref, RangeInclusive},
    sync::Arc,
};

pub use futures::stream::BoxStream;
pub use sqlx::types::chrono::{DateTime, Utc};

use crate::types::{BlockSubmission, L1Tx, NonEmptyVec, NonNegative, TransactionState};

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
    pub data: NonEmptyVec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuelBundle {
    pub id: NonNegative<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFragment {
    pub id: NonNegative<i32>,
    pub idx: NonNegative<i32>,
    pub bundle_id: NonNegative<i32>,
    pub data: NonEmptyVec<u8>,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequentialFuelBlocks {
    blocks: NonEmptyVec<FuelBlock>,
}

impl IntoIterator for SequentialFuelBlocks {
    type Item = FuelBlock;
    type IntoIter = <NonEmptyVec<FuelBlock> as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        self.blocks.into_iter()
    }
}

impl Deref for SequentialFuelBlocks {
    type Target = NonEmptyVec<FuelBlock>;
    fn deref(&self) -> &Self::Target {
        &self.blocks
    }
}

impl SequentialFuelBlocks {
    pub fn into_inner(self) -> NonEmptyVec<FuelBlock> {
        self.blocks
    }

    pub fn from_first_sequence(blocks: NonEmptyVec<FuelBlock>) -> Self {
        let blocks: Vec<_> = blocks
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
            .collect();

        let non_empty_blocks = NonEmptyVec::try_from(blocks).expect("at least the first block");

        non_empty_blocks.try_into().expect("blocks are sequential")
    }

    pub fn len(&self) -> NonZeroUsize {
        self.blocks.len()
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

// TODO: segfault needs testing
impl TryFrom<NonEmptyVec<FuelBlock>> for SequentialFuelBlocks {
    type Error = InvalidSequence;

    fn try_from(blocks: NonEmptyVec<FuelBlock>) -> std::result::Result<Self, Self::Error> {
        let vec = blocks.inner();

        let is_sorted = vec.windows(2).all(|w| w[0].height < w[1].height);
        if !is_sorted {
            return Err(InvalidSequence::new(
                "blocks are not sorted by height".to_string(),
            ));
        }

        let is_sequential = vec.windows(2).all(|w| w[0].height + 1 == w[1].height);
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
    async fn insert(&self, submission: BlockSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission>;
    async fn insert_block(&self, block: FuelBlock) -> Result<()>;
    async fn is_block_available(&self, hash: &[u8; 32]) -> Result<bool>;
    async fn available_blocks(&self) -> Result<Option<RangeInclusive<u32>>>;
    async fn lowest_sequence_of_unbundled_blocks(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Option<SequentialFuelBlocks>>;
    async fn insert_bundle_and_fragments(
        &self,
        block_range: RangeInclusive<u32>,
        fragments: NonEmptyVec<NonEmptyVec<u8>>,
    ) -> Result<NonEmptyVec<BundleFragment>>;

    async fn record_pending_tx(
        &self,
        tx_hash: [u8; 32],
        fragment_id: NonNegative<i32>,
    ) -> Result<()>;
    async fn get_pending_txs(&self) -> Result<Vec<L1Tx>>;
    async fn has_pending_txs(&self) -> Result<bool>;
    async fn oldest_nonfinalized_fragment(&self) -> Result<Option<BundleFragment>>;
    async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>>;
    async fn update_tx_state(&self, hash: [u8; 32], state: TransactionState) -> Result<()>;
}

impl<T: Storage + Send + Sync> Storage for Arc<T> {
    delegate! {
        to (self.as_ref()) {
            async fn insert(&self, submission: BlockSubmission) -> Result<()>;
                async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
                async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission>;
                async fn insert_block(&self, block: FuelBlock) -> Result<()>;
                async fn is_block_available(&self, hash: &[u8; 32]) -> Result<bool>;
                async fn available_blocks(&self) -> Result<Option<RangeInclusive<u32>>>;
                async fn lowest_sequence_of_unbundled_blocks(
                    &self,
                    starting_height: u32,
                    limit: usize,
                ) -> Result<Option<SequentialFuelBlocks>>;
                async fn insert_bundle_and_fragments(
                    &self,
                    block_range: RangeInclusive<u32>,
                    fragments: NonEmptyVec<NonEmptyVec<u8>>,
                ) -> Result<NonEmptyVec<BundleFragment>>;

                async fn record_pending_tx(
                    &self,
                    tx_hash: [u8; 32],
                    fragment_id: NonNegative<i32>,
                ) -> Result<()>;
                async fn get_pending_txs(&self) -> Result<Vec<L1Tx>>;
                async fn has_pending_txs(&self) -> Result<bool>;
                async fn oldest_nonfinalized_fragment(&self) -> Result<Option<BundleFragment>>;
                async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>>;
                async fn update_tx_state(&self, hash: [u8; 32], state: TransactionState) -> Result<()>;
        }
    }
}
