use std::{
    collections::{BTreeSet, HashSet},
    ops::Range,
    sync::Arc,
};

use futures::SinkExt;
use sqlx::types::chrono::{DateTime, Utc};

use crate::types::{BlockSubmission, L1Tx, NonNegative, StateSubmission, TransactionState};

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
    pub data: Vec<u8>,
}

impl From<crate::fuel::FuelBlock> for FuelBlock {
    fn from(value: crate::fuel::FuelBlock) -> Self {
        let data = value
            .transactions
            .into_iter()
            .flat_map(|tx| tx.into_iter())
            .collect();
        Self {
            hash: *value.id,
            height: value.header.height,
            data,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[async_trait::async_trait]
#[impl_tools::autoimpl(for<T: trait> &T, &mut T, Arc<T>, Box<T>)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Storage: Send + Sync {
    async fn insert(&self, submission: BlockSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission>;
    async fn insert_block(&self, block: FuelBlock) -> Result<()>;
    async fn is_block_available(&self, hash: &[u8; 32]) -> Result<bool>;
    async fn available_blocks(&self) -> Result<ValidatedRange>;
    async fn all_blocks(&self) -> Result<Vec<FuelBlock>>;
    async fn insert_bundle_and_fragments(
        &self,
        bundle_blocks: &[[u8; 32]],
        fragments: Vec<Vec<u8>>,
    ) -> Result<()>;

    // async fn insert_state_submission(&self, submission: StateSubmission) -> Result<()>;
    // fn stream_unfinalized_segment_data<'a>(
    //     &'a self,
    // ) -> Pin<Box<dyn Stream<Item = Result<UnfinalizedSubmissionData>> + 'a + Send>>;
    // async fn record_pending_tx(
    //     &self,
    //     tx_hash: [u8; 32],
    //     fragments: Vec<StateFragment>,
    // ) -> Result<()>;
    async fn get_pending_txs(&self) -> Result<Vec<L1Tx>>;
    async fn has_pending_txs(&self) -> Result<bool>;
    async fn state_submission_w_latest_block(&self) -> Result<Option<StateSubmission>>;
    async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>>;
    async fn update_submission_tx_state(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedRange {
    range: Range<u32>,
}

impl ValidatedRange {
    pub fn into_inner(self) -> Range<u32> {
        self.range
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRange {
    range: Range<u32>,
}

impl std::error::Error for InvalidRange {}

impl std::fmt::Display for InvalidRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid range: {:?}", self.range)
    }
}

impl TryFrom<Range<u32>> for ValidatedRange {
    type Error = InvalidRange;

    fn try_from(value: Range<u32>) -> std::result::Result<Self, Self::Error> {
        if value.start > value.end {
            return Err(InvalidRange { range: value });
        }

        Ok(Self { range: value })
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
