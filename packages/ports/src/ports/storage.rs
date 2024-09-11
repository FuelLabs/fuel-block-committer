use std::{fmt::Display, ops::Range, sync::Arc};

pub use futures::stream::BoxStream;
use sqlx::types::chrono::{DateTime, Utc};

use crate::types::{
    BlockSubmission, L1Tx, NonEmptyVec, NonNegative, StateSubmission, TransactionState, VecIsEmpty,
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
    pub data: Vec<u8>,
}

impl TryFrom<crate::fuel::FuelBlock> for FuelBlock {
    type Error = VecIsEmpty;
    fn try_from(value: crate::fuel::FuelBlock) -> std::result::Result<Self, Self::Error> {
        let tx_bytes: Vec<u8> = value
            .transactions
            .into_iter()
            .flat_map(|tx| tx.into_iter())
            .collect();

        let data = NonEmptyVec::try_from(tx_bytes)?;

        Ok(Self {
            hash: *value.id,
            height: value.header.height,
            data,
        })
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[async_trait::async_trait]
#[impl_tools::autoimpl(for<T: trait> &T, &mut T, Arc<T>, Box<T>)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Storage: Send + Sync {
    async fn insert(&self, submission: BlockSubmission) -> Result<()>;
    async fn all_fragments(&self) -> Result<Vec<BundleFragment>>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission>;
    async fn insert_block(&self, block: FuelBlock) -> Result<()>;
    async fn is_block_available(&self, hash: &[u8; 32]) -> Result<bool>;
    async fn available_blocks(&self) -> Result<ValidatedRange<u32>>;
    async fn all_blocks(&self) -> Result<Vec<FuelBlock>>;
    fn stream_unbundled_blocks(&self) -> BoxStream<Result<FuelBlock>, '_>;
    async fn insert_bundle_and_fragments(
        &self,
        block_range: ValidatedRange<u32>,
        fragments: NonEmptyVec<NonEmptyVec<u8>>,
    ) -> Result<NonEmptyVec<BundleFragment>>;

    // async fn insert_state_submission(&self, submission: StateSubmission) -> Result<()>;
    // fn stream_unfinalized_segment_data<'a>(
    //     &'a self,
    // ) -> Pin<Box<dyn Stream<Item = Result<UnfinalizedSubmissionData>> + 'a + Send>>;
    async fn record_pending_tx(
        &self,
        tx_hash: [u8; 32],
        fragment_id: NonNegative<i32>,
    ) -> Result<()>;
    async fn get_pending_txs(&self) -> Result<Vec<L1Tx>>;
    async fn has_pending_txs(&self) -> Result<bool>;
    async fn oldest_nonfinalized_fragment(&self) -> Result<Option<BundleFragment>>;
    async fn state_submission_w_latest_block(&self) -> Result<Option<StateSubmission>>;
    async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>>;
    async fn update_tx_state(&self, hash: [u8; 32], state: TransactionState) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedRange<NUM> {
    range: Range<NUM>,
}

impl<NUM> ValidatedRange<NUM> {
    pub fn contains(&self, value: NUM) -> bool
    where
        NUM: PartialOrd,
    {
        self.range.contains(&value)
    }

    pub fn inner(&self) -> &Range<NUM> {
        &self.range
    }

    pub fn into_inner(self) -> Range<NUM> {
        self.range
    }
}

impl<NUM: PartialOrd + Display> TryFrom<Range<NUM>> for ValidatedRange<NUM> {
    type Error = InvalidRange<NUM>;

    fn try_from(range: Range<NUM>) -> std::result::Result<Self, Self::Error> {
        if range.start > range.end {
            return Err(InvalidRange { range });
        }

        Ok(Self { range })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRange<NUM> {
    range: Range<NUM>,
}

impl<NUM: std::fmt::Debug> std::error::Error for InvalidRange<NUM> {}

impl<NUM: std::fmt::Debug> std::fmt::Display for InvalidRange<NUM> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid range: {:?}", self.range)
    }
}

// impl BlockRoster {
//     pub fn try_new(lowest: u32, highest: u32) -> Result<Self> {
//         if highest < lowest {
//             return Err(Error::Conversion(format!(
//                 "invalid block roster: highest({highest}) < lowest({lowest})"
//             )));
//         }
//
//         Ok(Self { lowest, highest })
//     }
//
//     pub fn missing_block_heights(
//         &self,
//         current_height: u32,
//         must_have_last_n_blocks: u32,
//     ) -> BTreeSet<u32> {
//         let mut missing = BTreeSet::from_iter(self.missing.clone());
//
//         if let Some((min, max)) = self.min_max_db_height {
//             missing.extend((max + 1)..=current_height);
//
//             if let Some(required_minimum_height) = required_minimum_height {
//                 missing.extend((required_minimum_height)..=min);
//             }
//         } else if let Some(required_minimum_height) = required_minimum_height {
//             missing.extend(0..required_minimum_height);
//         }
//
//         missing.retain(|&height| height >= lower_cutoff);
//
//         missing
//     }
// }

// #[cfg(test)]
// mod tests {
//     use fuel_core_client::client::schema::schema::__fields::Header::height;
//
//     use super::*;
//
//     macro_rules! set {
//         ( $( $x:expr ),* ) => {
//             {
//             let mut set = std::collections::BTreeSet::new();
//             $(
//                 set.insert($x);
//             )*
//             set
//             }
//         };
//     }
//
//     #[test]
//     fn lowest_cannot_be_higher_than_highest() {
//         // given
//         let highest = 10u32;
//         let lowest = 11u32;
//         let missing = vec![];
//
//         // when
//         let err =
//             BlockRoster::try_new(missing, Some((lowest, highest))).expect_err("should have failed");
//
//         // then
//         let Error::Conversion(err) = err else {
//             panic!("unexpected error: {}", err);
//         };
//         assert_eq!(err, "invalid block roster: highest(10) < lowest(11)");
//     }
//
//     #[test]
//     fn reports_no_missing_blocks() {
//         // given
//         let roster = BlockRoster::try_new(0, 10).unwrap();
//
//         // when
//         let missing = roster.missing_block_heights(10, 0, None);
//
//         // then
//         assert!(missing.is_empty());
//     }
//
//     #[test]
//     fn reports_what_the_db_gave() {
//         // given
//         let roster = BlockRoster::try_new(vec![1, 2, 3], Some((0, 10))).unwrap();
//
//         // when
//         let missing = roster.missing_block_heights(10, 0, None);
//
//         // then
//         assert_eq!(missing, set![1, 2, 3]);
//     }
//
//     #[test]
//     fn reports_missing_blocks_if_latest_height_doest_match_with_highest_db_block() {
//         // given
//         let roster = BlockRoster::try_new(vec![1, 2, 3], Some((0, 10))).unwrap();
//
//         // when
//         let missing = roster.missing_block_heights(12, 0, None);
//
//         // then
//         assert_eq!(missing, set![1, 2, 3, 11, 12]);
//     }
//
//     #[test]
//     fn wont_report_below_cutoff() {
//         // given
//         let roster = BlockRoster::try_new(vec![1, 2, 3], Some((0, 10))).unwrap();
//
//         // when
//         let missing = roster.missing_block_heights(12, 10, None);
//
//         // then
//         assert_eq!(missing, set![11, 12]);
//     }
//
//     #[test]
//     fn no_block_was_imported_ie_initial_db_state() {
//         // given
//         let roster = BlockRoster::try_new(vec![], None).unwrap();
//
//         // when
//         let missing = roster.missing_block_heights(10, 3, Some(4));
//
//         // then
//         assert_eq!(missing, set![4, 5, 6, 7, 8, 9, 10]);
//     }
// }
